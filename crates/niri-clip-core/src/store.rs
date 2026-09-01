use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
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
    crate::migrate::migrate_legacy_db(&path)?;
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
    crate::migrate::migrate_schema(&conn)?;
    Ok(conn)
}

#[cfg(unix)]
pub(crate) fn tighten_dir_perms(p: &Path) {
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

/// 图片磁盘配额 GC（路线图 1.3）：images/ 总量超过 max_image_total_bytes 时，
/// 按时间戳 LRU（最旧优先）整行淘汰图片条目，行删文件也删（同
/// enforce_max_items 的联动语义）。保护两类条目：星标（pinned=0 过滤）
/// 与当前项（state/current 指针 ≈ Ctrl+V 会粘出的内容，删了会粘出空气）。
/// 0 = 不限制。daemon 启动时随 prune_orphan_images 一并执行一次，
/// 运行期不重复触发（图片入库频率低，避免捕获路径额外开销）。
pub fn gc_images(max_bytes: u64) -> Result<usize> {
    if max_bytes == 0 {
        return Ok(0);
    }
    let conn = connect()?;
    let cur_hash = current_hash().unwrap_or_default();
    // size 列对图片条目即数据文件字节数（见 insert_image_with），
    // SUM 即 images/ 目录总量
    let total: i64 = conn.query_row(
        "SELECT COALESCE(SUM(size),0) FROM clips WHERE image_path IS NOT NULL",
        [],
        |r| r.get(0),
    )?;
    let mut need: i128 = total as i128 - max_bytes as i128;
    if need <= 0 {
        return Ok(0);
    }
    let mut n = 0usize;
    while need > 0 {
        // 逐条淘汰最旧的可淘汰图片行（ts ASC, id ASC：同毫秒内先入先出）。
        // 候选数远小于库规模，逐条成本可忽略；批量 DELETE 需按字节累计，
        // 复杂度不划算。可淘汰集合为空时提前收手（全受保护时宁超配额不丢数据）
        let victim: Option<(i64, Option<String>, i64)> = conn
            .query_row(
                "SELECT id, image_path, size FROM clips
                 WHERE image_path IS NOT NULL AND pinned=0 AND hash != ?1
                 ORDER BY ts ASC, id ASC LIMIT 1",
                params![cur_hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        let Some((id, path, size)) = victim else {
            break;
        };
        conn.execute("DELETE FROM clips WHERE id=?1", params![id])?;
        if let Some(p) = path {
            let _ = std::fs::remove_file(p);
        }
        need -= size as i128;
        n += 1;
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

/// 全库搜索结果集上限：GUI 渲染侧另有 MAX_RENDER_ROWS 兑底，
/// 这里限制的是 FTS 候选集大小（相关度取前 N）
pub const SEARCH_LIMIT: usize = 300;

/// FTS5 全文搜索（任务 2.1，trigram tokenizer：中英文子串均命中）。
///
/// * query ≥3 字符（chars 计）→ clips_fts MATCH 短语查询，bm25 相关度排序
///   （trigram 索引要求查询至少 3 字符才能命中）
/// * 更短查询 → clips.text LIKE 线性扫描（10k 条毫秒级，trigram 索引对
///   短查询无增益）
/// * 并列时新者优先；空查询返回空集
///
/// 查询内的双引号翻倍转义，防止用户输入破坏 MATCH 短语语法（参数数组
/// 走 bind，无注入面；这里只是 FTS 查询语法层的问题）
pub fn search(query: &str, limit: usize) -> Result<Vec<Clip>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let conn = connect()?;
    let lim = limit as i64;
    let mut out = Vec::new();
    if q.chars().count() >= 3 {
        let phrase = format!("\"{}\"", q.replace('"', "\"\""));
        let mut stmt = conn.prepare(
            // 注：FTS5 的 MATCH 左侧必须是 fts 表名本身，别名会被当列名解析报错
            "SELECT c.id, c.hash, c.text, c.mime, c.pinned, c.image_path
             FROM clips_fts JOIN clips c ON c.id = clips_fts.rowid
             WHERE clips_fts MATCH ?1
             ORDER BY bm25(clips_fts), c.ts DESC, c.id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![phrase, lim], row_to_clip)?;
        for r in rows {
            out.push(r?);
        }
    } else {
        // LIKE 通配符转义：用户输入中的 % _ \ 均按字面匹配
        let pat = format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let sql = format!(
            "SELECT {CLIP_COLS} FROM clips
             WHERE text LIKE ?1 ESCAPE '\\'
             ORDER BY ts DESC, id DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![pat, lim], row_to_clip)?;
        for r in rows {
            out.push(r?);
        }
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
#[path = "store_tests.rs"]
mod tests;
