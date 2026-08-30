use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;

// v0.4：移除进程内 200ms 缓存层。fzf 每次 reload-sync 都会 spawn 全新的
// `niri-clip list-raw` 进程，OnceLock 进程内缓存从未在该路径生效；
// daemon 进程内的 invalidate 也无法触达其它进程。实测 list 300 <11ms，
// 直查即可，复杂度偿还。相关入口收敛为 `list(min(max_items, TUI_LIMIT))`。
pub const TUI_LIMIT: usize = 300;
const BUSY_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, Clone)]
pub struct Clip {
    pub id: i64,
    /// 去重指纹：TUI/GUI 的 ▶ 当前项标记与 copy 后指针刷新都依赖
    pub hash: String,
    pub text: String,
    pub mime: String,
    pub pinned: bool,
    /// v0.4：图片条目对应的数据文件（images/{id}.bin），修复预览错位的关键字段
    pub image_path: Option<String>,
}

#[derive(Debug)]
pub struct InsertedImage {
    pub id: i64,
    pub path: PathBuf,
}

fn connect() -> Result<Connection> {
    let path = Config::db_path();
    migrate_legacy_db(&path)?;
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
        tighten_dir_perms(p);
    }
    let conn = Connection::open(&path).context("open sqlite")?;
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
        ",
    )?;
    tighten_file_perms(&path);
    migrate_schema(&conn)?;
    Ok(conn)
}

/// v0.4：数据库从 ~/.cache 迁往 ~/.local/state（XDG state 规范——
/// 剪贴板历史属于应持久的状态数据，放在 ~/.cache 会被系统清理工具误删）。
/// 首次发现旧库且新库缺失时，用 `VACUUM INTO` 做一致性快照搬迁，
/// 旧库保留作为备份不动。
fn migrate_legacy_db(new_path: &Path) -> Result<()> {
    let legacy = Config::legacy_db_path();
    if legacy == new_path || !legacy.exists() || new_path.exists() {
        return Ok(());
    }
    if let Some(p) = new_path.parent() {
        std::fs::create_dir_all(p)?;
        tighten_dir_perms(p);
    }
    let src = Connection::open_with_flags(&legacy, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("open legacy niri-clip db")?;
    let res = src.execute("VACUUM INTO ?1", [new_path.to_string_lossy().as_ref()]);
    match res {
        Ok(_) => eprintln!(
            "[niri-clip] 已迁移历史数据库到 {}（原 {} 保留备份）",
            new_path.display(),
            legacy.display()
        ),
        Err(e) => {
            // 多进程同时首次启动时的 TOCTOU：对方已迁移成功则继续
            if new_path.exists() {
                eprintln!("[niri-clip] 旧库已由其他实例完成迁移");
            } else {
                return Err(anyhow!("migrate db {:?} -> {:?}: {e}", legacy, new_path));
            }
        }
    }
    Ok(())
}

/// v0.4：引入 PRAGMA user_version 迁移机制（此前 schema 演进没有版本标记）。
/// 版本 0：建基表/索引，删除从未参与查询的 FTS 占位表
/// 版本 1 -> 2：新增 image_path 列（图片条目与数据文件按 clip id 关联）
fn migrate_schema(conn: &Connection) -> Result<()> {
    let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ver < 1 {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS clips (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                hash TEXT UNIQUE,
                text TEXT NOT NULL,
                mime TEXT DEFAULT 'text/plain',
                ts INTEGER NOT NULL,
                pinned INTEGER DEFAULT 0,
                size INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_hash ON clips(hash);
            CREATE INDEX IF NOT EXISTS idx_pinned_ts ON clips(pinned DESC, ts DESC);
            DROP TABLE IF EXISTS clips_fts;
            ",
        )?;
        conn.execute_batch("PRAGMA user_version=1;")?;
    }
    if ver < 2 {
        let has_col: i64 = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('clips') WHERE name='image_path'",
            [],
            |r| r.get(0),
        )?;
        if has_col == 0 {
            conn.execute_batch("ALTER TABLE clips ADD COLUMN image_path TEXT;")?;
        }
        conn.execute_batch("PRAGMA user_version=2;")?;
    }
    Ok(())
}

#[cfg(unix)]
fn tighten_dir_perms(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn tighten_dir_perms(_p: &Path) {}

#[cfg(unix)]
fn tighten_file_perms(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn tighten_file_perms(_p: &Path) {}

/// 文本去重指纹（daemon 轮询短路复用）。注：DefaultHasher 算法不受稳定性保证，
/// 本轮暂不更换以免存量 hash 失配导致整表翻倍重插；图片走稳定的 fnv64
/// （见 insert_image），待 v1.0 计划内统一（需伴随一次性 hash 重算）。
pub fn hash_text(s: &str) -> String {
    // 简单 blake3 替代：用 std hash + 长度，避免额外依赖
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}-{}", h.finish(), s.len())
}

/// FNV-1a 64：跨进程/跨编译器版本稳定的内容指纹，用于图片二进制去重
fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// daemon 轮询短路用的图片内容 key，与 insert_image 的 hash 同源。
/// 指纹 = mime + FNV64 + 字节长度：同字节数据以不同 mime 复制时视为不同条目
pub fn image_content_key(mime: &str, bytes: &[u8]) -> String {
    format!("img:{mime}:{:x}-{}", fnv64(bytes), bytes.len())
}

pub fn should_ignore(text: &str, cfg: &Config) -> bool {
    if text.chars().count() < cfg.min_store_length {
        return true;
    }
    // 编译产物随 Config::load 缓存（见 config.rs ignore_re）；None = 模式非法，不过滤
    if let Some(re) = &cfg.ignore_re {
        if re.is_match(text) {
            return true;
        }
    }
    false
}

// =====================================================================
// v0.5：当前项指针（current pointer）
//
// state/current 单行文件记录"最后一次被成功捕获的内容 hash"。语义：
// ▶ 标识 = 你最后一次复制的东西 ≈ Ctrl+V 会粘出的内容。
// - 仅在捕获成功时刷新（新入库 / 去重刷 ts）；被 ignore_regex 过滤、
//   超过体积上限、空载荷均不写 → "当前剪贴板不在历史中"由指针与列表
//   不匹配自然表达，不会撒谎。
// - list() 依此把当前项排到第 1 行（星标之上），fzf/fuzzel 行首打 ▶。
// =====================================================================

fn current_pointer_path() -> PathBuf {
    Config::state_dir().join("current")
}

/// 读取当前项指针；文件缺失或为空返回 None（旧库升级后自然无指针）
pub fn current_hash() -> Option<String> {
    std::fs::read_to_string(current_pointer_path())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 刷新当前项指针（供捕获路径与 TUI copy 路径调用）。
/// 指针属辅助功能，写失败不阻断捕获，仅记 stderr。
pub fn touch_current(hash: &str) {
    if let Err(e) = std::fs::write(current_pointer_path(), hash) {
        eprintln!("[niri-clip] write current pointer failed: {e}");
    }
}

/// 入库文本（自带配置加载）。便利入口供 CLI/测试使用；
/// daemon 等已持有 Config 的调用方请用 `insert_with` 免去重复读盘解析。
pub fn insert(text: String, mime: Option<String>) -> Result<bool> {
    insert_with(text, mime, &Config::load())
}

/// 同 insert，但复用调用方已加载的配置（捕获热路径上 Config::load 的
/// 同步读盘 + TOML 解析是每次捕获都要付的成本，能省则省）
pub fn insert_with(text: String, mime: Option<String>, cfg: &Config) -> Result<bool> {
    // 统一空白语义：所有捕获路径（watch 管道 / try_system_capture / native
    // 轮询）必须在同一 hash 口径下去重。此前仅 try_system_capture 做 trim，
    // 同一次剪贴板变化的竞态双触发会以"原文版 + trim 版"两份入库（真实库
    // 可见 ts 仅差 40ms、长度差首尾空白的成对条目），TUI Enter 复制后即
    // 表现为多出一条"带 ↵/空格"的孪生记录。
    let text = text.trim().to_string();
    if text.is_empty() {
        return Ok(false);
    }
    if should_ignore(&text, cfg) {
        return Ok(false);
    }
    // v0.5：单条体积上限。守卫放 store 层（单一真相源）——除 daemon 三个捕获
    // 路径外，migrate 等所有调用方同样受限；daemon 侧另有 Read::take 有界读
    // 保证读取过程本身的内存上限。
    if text.len() > cfg.max_clip_bytes {
        eprintln!(
            "[niri-clip store] 条目 {} 字节超过 max_clip_bytes={}，拒绝入库",
            text.len(),
            cfg.max_clip_bytes
        );
        return Ok(false);
    }
    let mut conn = connect()?;
    let hash = hash_text(&text);
    let ts = Utc::now().timestamp_millis();
    let mime = mime.unwrap_or_else(|| "text/plain".to_string());
    let size = text.len() as i64;

    // BEGIN IMMEDIATE：SELECT 去重检查 + INSERT 必须原子化。否则多进程并发时
    // （典型场景：fzf 选中旧条目 -> wl-copy 写回 -> daemon 轮询捕获同一 hash）
    // 双双通过检查后一方撞 UNIQUE 报错并被静默吞掉。
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: Option<i64> = tx
        .query_row("SELECT id FROM clips WHERE hash=?1", params![hash], |r| {
            r.get(0)
        })
        .ok();
    if let Some(id) = exists {
        tx.execute("UPDATE clips SET ts=?1 WHERE id=?2", params![ts, id])?;
        tx.commit()?;
        // 重复捕获同样代表"剪贴板变成了这个内容"：刷新指针
        touch_current(&hash);
        return Ok(false);
    }
    tx.execute(
        "INSERT INTO clips (hash, text, mime, ts, size) VALUES (?1,?2,?3,?4,?5)",
        params![hash, text, mime, ts, size],
    )?;
    tx.commit()?;
    touch_current(&hash);

    enforce_max_items(&conn, cfg.max_items)?;
    Ok(true)
}

/// v0.4：入库图片剪贴板。
/// - 内容 key 改用 fnv64+len：修复此前 `img-{mime}-{len}` 对相同字节长度的不同
///   图片误判重的问题（两张等大 PNG 只会收录第一张）
/// - 二进制写 `images/{id}.bin` 并把路径记入 clips.image_path，预览按条目精确读取
/// - 重复内容返回 None：仅刷新时间戳，文件与关联不变
///
/// 注意：图片不做 ignore_regex 内容过滤（无法对二进制语义扫描）；上限裁剪共用。
pub fn insert_image(mime: &str, bytes: &[u8]) -> Result<Option<InsertedImage>> {
    insert_image_with(mime, bytes, &Config::load())
}

/// 同 insert_image，但复用调用方已加载的配置（见 insert_with）
pub fn insert_image_with(mime: &str, bytes: &[u8], cfg: &Config) -> Result<Option<InsertedImage>> {
    // v0.5：图片单张体积上限（截图通常 1–3MB，默认 10MiB 给足余量）
    if bytes.len() > cfg.max_image_bytes {
        eprintln!(
            "[niri-clip store] 图片 {} 字节超过 max_image_bytes={}，拒绝入库",
            bytes.len(),
            cfg.max_image_bytes
        );
        return Ok(None);
    }
    let mut conn = connect()?;
    let hash = image_content_key(mime, bytes);
    let ts = Utc::now().timestamp_millis();
    let placeholder = format!("[image {} {} bytes]", mime, bytes.len());

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let exists: Option<i64> = tx
        .query_row("SELECT id FROM clips WHERE hash=?1", params![hash], |r| {
            r.get(0)
        })
        .ok();
    if let Some(id) = exists {
        tx.execute("UPDATE clips SET ts=?1 WHERE id=?2", params![ts, id])?;
        tx.commit()?;
        touch_current(&hash);
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO clips (hash, text, mime, ts, size) VALUES (?1,?2,?3,?4,?5)",
        params![hash, placeholder, mime, ts, bytes.len() as i64],
    )?;
    let id = tx.last_insert_rowid();

    // 数据文件写入放进同一事务窗口：先落 `.tmp-` 再原子 rename 到最终路径。
    // 任何一步失败 tx drop 即回滚行，不再出现"有行无图"的永久残缺状态
    // （旧行为：先 commit 行、后写文件，中途崩溃则 hash 已占用，该图永远无法重录）。
    // rename 成功但 UPDATE 失败的残余文件由 prune_orphan_images 兜底回收。
    let dir = Config::images_dir();
    std::fs::create_dir_all(&dir)?;
    tighten_dir_perms(&dir);
    let path = dir.join(format!("{}.bin", id));
    let tmp = dir.join(format!(".tmp-{}.bin", id));
    std::fs::write(&tmp, bytes).with_context(|| format!("write image cache {}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("publish image cache {}", path.display()))?;
    tighten_file_perms(&path);
    tx.execute(
        "UPDATE clips SET image_path=?1 WHERE id=?2",
        params![path.to_string_lossy(), id],
    )?;
    tx.commit()?;
    touch_current(&hash);

    enforce_max_items(&conn, cfg.max_items)?;
    Ok(Some(InsertedImage { id, path }))
}

fn enforce_max_items(conn: &Connection, max_items: usize) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;
    if count > max_items as i64 {
        let to_del = count - max_items as i64;
        // RETURNING 带出被淘汰条目的数据文件路径，行删文件也删（否则
        // images/ 只进不出，截图类负载下 state 目录会无限膨胀）
        let mut stmt = conn.prepare(
            "DELETE FROM clips WHERE id IN (SELECT id FROM clips WHERE pinned=0 ORDER BY ts ASC LIMIT ?1) RETURNING image_path",
        )?;
        let paths = stmt.query_map(params![to_del], |r| r.get::<_, Option<String>>(0))?;
        for p in paths {
            if let Some(p) = p? {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    Ok(())
}

/// 孤儿清扫：回收 images/ 下不被任何 clips.image_path 引用的数据文件。
/// 两个来源：v0.5.0 及更早版本 delete/wipe/淘汰只删行不删文件的存量残留；
/// insert_image 在 rename 成功但 UPDATE 失败窗口内留下的无主文件。
/// `.tmp-` 前缀为入库中途崩溃的临时文件，一并清理。daemon 启动时调用一次。
pub fn prune_orphan_images() -> Result<usize> {
    let conn = connect()?;
    let dir = Config::images_dir();
    if !dir.exists() {
        return Ok(0);
    }
    let referenced: std::collections::HashSet<String> = {
        let mut stmt = conn.prepare("SELECT image_path FROM clips WHERE image_path IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for r in rows {
            set.insert(r?);
        }
        set
    };
    let mut n = 0;
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let p = entry.path();
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.starts_with(".tmp-") && referenced.contains(&p.to_string_lossy().to_string()) {
            continue;
        }
        if std::fs::remove_file(&p).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

const CLIP_COLS: &str = "id, hash, text, mime, pinned, image_path";

fn row_to_clip(r: &rusqlite::Row<'_>) -> rusqlite::Result<Clip> {
    Ok(Clip {
        id: r.get(0)?,
        hash: r.get(1)?,
        text: r.get(2)?,
        mime: r.get(3)?,
        pinned: r.get::<_, i64>(4)? != 0,
        image_path: r.get(5)?,
    })
}

pub fn list(limit: usize) -> Result<Vec<Clip>> {
    let cfg = Config::load();
    let conn = connect()?;
    // 当前项永远第 1 行（星标之上）：第 1 行 = Ctrl+V 会粘出的内容。
    // 无指针时绑空串（hash 列不存在空值），排序退化为原行为。
    let cur_hash = current_hash().unwrap_or_default();
    let order = if cfg.pinned_on_top {
        "(hash = ?2) DESC, pinned DESC, ts DESC, id DESC"
    } else {
        "(hash = ?2) DESC, ts DESC, id DESC"
    };
    let sql = format!("SELECT {CLIP_COLS} FROM clips ORDER BY {order} LIMIT ?1");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit as i64, cur_hash], row_to_clip)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn delete(id: i64) -> Result<()> {
    let conn = connect()?;
    let img: Option<String> = conn
        .query_row(
            "SELECT image_path FROM clips WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .ok();
    conn.execute("DELETE FROM clips WHERE id=?1", params![id])?;
    // 行删文件也删；文件清理失败不阻断（残留由 prune_orphan_images 兜底）
    if let Some(p) = img {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

pub fn wipe() -> Result<()> {
    let conn = connect()?;
    conn.execute("DELETE FROM clips", [])?;
    // 全库清空后 images/ 下不再有任何引用者，整个目录内容可安全清空
    if let Ok(rd) = std::fs::read_dir(Config::images_dir()) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    Ok(())
}

pub fn toggle_pin(id: i64) -> Result<bool> {
    let conn = connect()?;
    let cur: i64 = conn.query_row("SELECT pinned FROM clips WHERE id=?1", params![id], |r| {
        r.get(0)
    })?;
    let new = if cur == 0 { 1 } else { 0 };
    conn.execute("UPDATE clips SET pinned=?1 WHERE id=?2", params![new, id])?;
    Ok(new == 1)
}

pub fn is_pinned(id: i64) -> Result<bool> {
    let conn = connect()?;
    let v: i64 = conn.query_row("SELECT pinned FROM clips WHERE id=?1", params![id], |r| {
        r.get(0)
    })?;
    Ok(v != 0)
}

pub fn get(id: i64) -> Result<Clip> {
    let conn = connect()?;
    let c = conn.query_row(
        &format!("SELECT {CLIP_COLS} FROM clips WHERE id=?1"),
        params![id],
        row_to_clip,
    )?;
    Ok(c)
}

/// 复制指定条目到剪贴板（wl-copy 子进程），并刷新当前项指针。
/// CLI `copy` 子命令与原生 UI 的 Enter/Ctrl-Y 共用此唯一路径，
/// 保证 ▶ 跟随语义在所有复制入口一致。
///
/// 图片条目：clips.text 只是 "[image mime N bytes]" 占位符，真实载荷在
/// images/{id}.bin——必须以 `wl-copy --type {mime}` 灌入文件字节。
/// 此前一律写 text，把占位文本顶进剪贴板（还顺带毁掉当前真实的截图），
/// 粘贴出来的是一行字而不是图。
pub fn copy_to_clipboard(id: i64) -> Result<()> {
    let clip = get(id)?;
    if clip.mime.starts_with("image/") {
        let path = clip
            .image_path
            .as_deref()
            .context("图片条目缺少数据文件路径")?;
        let file = std::fs::File::open(path).with_context(|| format!("open {path}"))?;
        let mut wl = std::process::Command::new("wl-copy")
            .arg(format!("--type={}", clip.mime))
            // wl-copy 直接读 fd，无需内存中转 stdin 管道
            .stdin(std::process::Stdio::from(file))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("wl-copy")?;
        wl.wait()?;
        touch_current(&clip.hash);
        return Ok(());
    }
    let mut wl = std::process::Command::new("wl-copy")
        .stdin(std::process::Stdio::piped())
        // wl-copy 会 fork 守护进程常驻服务剪贴板，不得持有调用方终端 fd
        // （详见 tui.rs 同款注释），重定向 null 释放 pty
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("wl-copy")?;
    wl.stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(clip.text.as_bytes())?;
    wl.wait()?;
    touch_current(&clip.hash);
    Ok(())
}

/// 从 cliphist 迁移（一次性）。导入借用 insert() 会顺带刷新当前项指针，
/// 但迁移导入的是旧历史、剪贴板并未变化——迁移前保存指针，结束后还原
/// （原本无指针则清除），避免 ▶ 指向最后一条导入的旧条目。
pub fn migrate_from_cliphist() -> Result<usize> {
    let saved_current = current_hash();
    let out = std::process::Command::new("cliphist").arg("list").output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return Ok(0),
    };
    let s = String::from_utf8_lossy(&out.stdout);
    let cfg = Config::load();
    let mut n = 0;
    for line in s.lines() {
        if let Some((id_str, preview)) = line.split_once('\t') {
            if let Ok(id) = id_str.parse::<i64>() {
                // 用 cliphist decode 拿全量文本
                let decoded = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(format!("echo {} | cliphist decode", id))
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_else(|| preview.to_string());
                if insert_with(decoded, None, &cfg)? {
                    n += 1;
                }
            }
        }
    }
    match saved_current {
        Some(h) => touch_current(&h),
        None => {
            let _ = std::fs::remove_file(current_pointer_path());
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// 目录级隔离 + 串行化：通过 XDG_* 环境变量把所有持久化位置指进临时目录，
    /// 测试互不影响且不会触碰真实用户目录。
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static SEQ: AtomicUsize = AtomicUsize::new(0);

    struct EnvGuard {
        prev_state: Option<String>,
        prev_config: Option<String>,
        prev_cache: Option<String>,
        root: PathBuf,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_var("XDG_STATE_HOME", &self.prev_state);
            restore_var("XDG_CONFIG_HOME", &self.prev_config);
            restore_var("XDG_CACHE_HOME", &self.prev_cache);
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn restore_var(key: &str, val: &Option<String>) {
        match val {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn with_env(f: impl FnOnce(&EnvGuard)) {
        let _g = ENV_LOCK.lock().unwrap();
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let root =
            std::env::temp_dir().join(format!("niri-clip-ut-{}-{}", std::process::id(), seq));
        std::fs::create_dir_all(root.join("state")).unwrap();

        let prev_state = std::env::var("XDG_STATE_HOME").ok();
        let prev_config = std::env::var("XDG_CONFIG_HOME").ok();
        let prev_cache = std::env::var("XDG_CACHE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", root.join("state"));
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_CACHE_HOME", root.join("cache"));

        let guard = EnvGuard {
            prev_state,
            prev_config,
            prev_cache,
            root,
        };
        f(&guard);
        drop(guard);
    }

    fn clear_db() {
        let p = Config::db_path();
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
    }

    #[test]
    fn should_ignore_filters_secrets_and_short_input() {
        let cfg = Config::default();
        assert!(should_ignore("my password is hunter2", &cfg));
        assert!(should_ignore("", &cfg), "空文本应被忽略");
        let cfg2 = Config {
            min_store_length: 5,
            ..Config::default()
        };
        assert!(should_ignore("ab", &cfg2));
        assert!(!should_ignore("hello world", &cfg));
        assert!(!should_ignore("plain text", &cfg));
    }

    #[test]
    fn upsert_dedups_same_hash_atomically() {
        with_env(|_| {
            clear_db();
            assert!(insert("dup-entry-a".into(), None).unwrap());
            assert!(!insert("dup-entry-a".into(), None).unwrap());
            let all = list(100).unwrap();
            let hits = all
                .iter()
                .filter(|c| c.text.contains("dup-entry-a"))
                .count();
            assert_eq!(hits, 1, "相同文本应只占一行");
        });
    }

    #[test]
    fn insert_trims_whitespace_and_dedups_variants() {
        with_env(|_| {
            clear_db();
            // 带首尾空白与纯净版视为同一 hash：消除 watch 管道与
            // try_system_capture 的 trim 语义分歧导致的孪生条目
            assert!(insert("dup-entry-x\n".into(), None).unwrap());
            assert!(!insert("dup-entry-x".into(), None).unwrap());
            assert!(!insert("  dup-entry-x  ".into(), None).unwrap());
            let all = list(100).unwrap();
            let hits = all.iter().filter(|c| c.text == "dup-entry-x").count();
            assert_eq!(hits, 1, "空白变体应只占一行且入库为 trim 后文本");
            // 纯空白不入库
            assert!(!insert("   \n\t ".into(), None).unwrap());
        });
    }

    #[test]
    fn busy_timeout_is_set_on_connection() {
        with_env(|_| {
            clear_db();
            let conn = connect().unwrap();
            let v: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
                .unwrap();
            assert_eq!(v, BUSY_TIMEOUT_MS as i64);
            let uv: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(uv, 2, "schema 应迁移到版本 2");
        });
    }

    #[test]
    fn image_insert_associates_data_file_and_dedups_by_content_not_length() {
        with_env(|_| {
            clear_db();
            let a = b"\x89PNG\r\n\x1a\n bytes-of-A ".to_vec();
            let img_a = insert_image("image/png", &a)
                .unwrap()
                .expect("first insert");
            let clip_a = get(img_a.id).unwrap();
            assert_eq!(
                clip_a.image_path.as_deref(),
                Some(img_a.path.to_string_lossy().as_ref()),
                "clip 行应记录自身数据文件路径"
            );
            assert!(img_a.path.exists());

            // 相同内容重复拷贝 -> 判重，仅刷时间戳
            let again = insert_image("image/png", &a).unwrap();
            assert!(again.is_none());

            // 相同字节长度但内容不同 -> 不得再因 len 判重（修复点）
            let mut b = a.clone();
            b[10] ^= 0xFF;
            let img_b = insert_image("image/png", &b)
                .unwrap()
                .expect("len equal but different bytes");
            assert_ne!(img_b.id, img_a.id);

            let conn = Connection::open(Config::db_path()).unwrap();
            let total: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM clips WHERE mime LIKE 'image/%'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(total, 2);
        });
    }

    #[test]
    fn insert_rejects_oversize_text_at_boundary() {
        with_env(|g| {
            clear_db();
            // 通过测试隔离环境写入小限额配置，Config::load() 在 insert 内生效
            let cfg_dir = g.root.join("config/niri-clip");
            std::fs::create_dir_all(&cfg_dir).unwrap();
            std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 64\n").unwrap();

            let at_limit = "x".repeat(64);
            assert!(insert(at_limit, None).unwrap(), "恰好达到上限应入库");
            let over = "y".repeat(65);
            assert!(!insert(over, None).unwrap(), "超限一个字节即拒绝");

            let all = list(10).unwrap();
            assert_eq!(all.len(), 1, "超限条目不得落库");
            assert!(all[0].text.starts_with('x'));
        });
    }

    #[test]
    fn insert_image_rejects_oversize_payload_at_boundary() {
        with_env(|g| {
            clear_db();
            let cfg_dir = g.root.join("config/niri-clip");
            std::fs::create_dir_all(&cfg_dir).unwrap();
            std::fs::write(cfg_dir.join("config.toml"), "max_image_bytes = 64\n").unwrap();

            let big = vec![0u8; 65];
            assert!(
                insert_image("image/png", &big).unwrap().is_none(),
                "超限图片应拒绝且不产生数据文件"
            );
            assert!(!Config::images_dir().join("1.bin").exists());

            let ok = vec![0u8; 64];
            let img = insert_image("image/png", &ok)
                .unwrap()
                .expect("恰好达到上限应入库");
            assert!(img.path.exists());
        });
    }

    #[test]
    fn current_pointer_tracks_capture_and_tops_list() {
        with_env(|_| {
            clear_db();
            insert("old-a".into(), None).unwrap();
            insert("old-b".into(), None).unwrap();
            assert!(current_hash().is_some(), "insert 成功即写指针");
            let all = list(10).unwrap();
            assert_eq!(all[0].text, "old-b", "最后捕获者置顶");

            // 星标压不过当前项：当前项永远第 1 行
            let pinned_id = all[1].id; // old-a
            toggle_pin(pinned_id).unwrap();
            assert_eq!(list(10).unwrap()[0].text, "old-b", "星标不得顶掉当前项");

            // 重复捕获（dedup 刷 ts 路径）同样刷新指针
            insert("old-a".into(), None).unwrap();
            let cur = current_hash().unwrap();
            assert_eq!(list(10).unwrap()[0].hash, cur, "▶ 应跟随最后一次捕获");
        });
    }

    #[test]
    fn oversize_or_ignored_capture_does_not_move_current_pointer() {
        with_env(|g| {
            clear_db();
            insert("keep-me".into(), None).unwrap();
            let cur = current_hash().unwrap();

            // 超限拒绝不写指针
            let cfg_dir = g.root.join("config/niri-clip");
            std::fs::create_dir_all(&cfg_dir).unwrap();
            std::fs::write(cfg_dir.join("config.toml"), "max_clip_bytes = 8\n").unwrap();
            insert("this is way beyond eight bytes".into(), None).unwrap();
            assert_eq!(current_hash().unwrap(), cur, "超限捕获不得移动 ▶");
            // ignore_regex 命中不写指针
            insert("my password is hunter2".into(), None).unwrap();
            assert_eq!(current_hash().unwrap(), cur, "被过滤捕获不得移动 ▶");
        });
    }

    #[test]
    fn legacy_cache_db_is_snapshotted_into_state_dir() {
        with_env(|g| {
            // 在旧的 ~/.cache 位置构造一个含数据的历史库
            let legacy = Config::legacy_db_path();
            std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
            {
                let lc = Connection::open(&legacy).unwrap();
                lc.execute_batch(
                    "CREATE TABLE clips (id INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT UNIQUE,
                     text TEXT NOT NULL, mime TEXT DEFAULT 'text/plain', ts INTEGER NOT NULL,
                     pinned INTEGER DEFAULT 0, size INTEGER);
                     INSERT INTO clips(hash, text, ts) VALUES ('legacy-1','old entry',1);",
                )
                .unwrap();
            }
            // 环境已把新状态目录指向临时区，任何一次 connect() 都应触发搬迁
            assert!(insert("new entry".into(), None).unwrap());
            let all = list(50).unwrap();
            assert!(
                all.iter().any(|c| c.text == "old entry"),
                "旧库条目应出现在迁移后的新库中"
            );
            assert!(
                Config::db_path().exists(),
                "新库应位于 {:?}",
                g.root.join("state")
            );
        });
    }

    #[test]
    fn pin_orders_first_and_list_respects_limit() {
        with_env(|_| {
            clear_db();
            for i in 0..5 {
                insert(format!("item-{i}"), None).unwrap();
            }
            let all = list(TUI_LIMIT).unwrap();
            let head_id = all[0].id;
            toggle_pin(head_id).unwrap();
            let after = list(TUI_LIMIT).unwrap();
            assert_eq!(after[0].id, head_id, "pinned 应置顶");
            assert!(after[0].pinned);
            let few = list(3).unwrap();
            assert_eq!(few.len(), 3);
        });
    }

    #[test]
    fn delete_and_wipe_remove_image_files() {
        with_env(|_| {
            clear_db();
            let a = insert_image("image/png", b"\x89PNG-a")
                .unwrap()
                .expect("insert a");
            let b = insert_image("image/png", b"\x89PNG-b")
                .unwrap()
                .expect("insert b");
            assert!(a.path.exists() && b.path.exists());
            delete(a.id).unwrap();
            assert!(!a.path.exists(), "delete 应同步删除数据文件");
            assert!(b.path.exists());
            wipe().unwrap();
            assert!(!b.path.exists(), "wipe 应清空所有数据文件");
        });
    }

    #[test]
    fn max_items_eviction_removes_image_files() {
        with_env(|g| {
            clear_db();
            let cfg_dir = g.root.join("config/niri-clip");
            std::fs::create_dir_all(&cfg_dir).unwrap();
            std::fs::write(cfg_dir.join("config.toml"), "max_items = 1\n").unwrap();
            let a = insert_image("image/png", b"\x89PNG-a")
                .unwrap()
                .expect("insert a");
            let b = insert_image("image/png", b"\x89PNG-b")
                .unwrap()
                .expect("insert b");
            // b 入库触发淘汰：a（未 pin、更旧）连行带文件一起消失
            assert!(get(a.id).is_err(), "淘汰条目的行应被删除");
            assert!(!a.path.exists(), "淘汰条目的数据文件应被删除");
            assert!(b.path.exists());
        });
    }

    #[test]
    fn prune_orphan_images_removes_unreferenced_files() {
        with_env(|_| {
            clear_db();
            let img = insert_image("image/png", b"\x89PNG-ok")
                .unwrap()
                .expect("insert");
            let dir = Config::images_dir();
            // 旧版本遗留的孤儿：文件在、行不在
            let orphan = dir.join("9999.bin");
            std::fs::write(&orphan, b"orphan").unwrap();
            // 入库中途崩溃的临时文件
            let tmp = dir.join(".tmp-1234.bin");
            std::fs::write(&tmp, b"tmp").unwrap();
            assert_eq!(prune_orphan_images().unwrap(), 2);
            assert!(!orphan.exists() && !tmp.exists());
            assert!(img.path.exists(), "被引用的文件不得误删");
            assert_eq!(prune_orphan_images().unwrap(), 0, "二次清扫应无残留");
        });
    }
}
