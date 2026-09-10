use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::Config;

// v0.4：移除进程内 200ms 缓存层。fzf 每次 reload-sync 都会 spawn 全新的
// `niri-clip list-raw` 进程，OnceLock 进程内缓存从未在该路径生效；
// daemon 进程内的 invalidate 也无法触达其它进程。实测 list 300 <11ms，
// 直查即可，复杂度偿还。相关入口收敛为 `list(min(max_items, MENU_LIMIT))`。
/// 列表窗口上限：菜单取数与渲染行数**共用**同一口径——fzf TUI 与原生 UI
/// 都取此值，保证"同一份历史，两个后端看到同样多的条目"（此前 TUI 取 300、
/// GUI 取 max_items=750，同一份历史两种窗口，属 2.6 收敛项）。
/// 全库搜索**不经此窗口**：`store::search` 走 FTS/LIKE 覆盖全库
pub const MENU_LIMIT: usize = 300;
/// 全库搜索候选集上限（按相关度/时间序取前 N）。与 `MENU_LIMIT` 数值相同
/// 但语义独立：前者限制"列表窗口"，后者限制"搜索候选"，不可互相替换
pub const SEARCH_LIMIT: usize = 300;
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

/// 连接（含旧库搬迁 + schema 迁移）。backup.rs（导出/回灌）同用此唯一入口
pub(crate) fn connect() -> Result<Connection> {
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
pub(crate) fn tighten_file_perms(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn tighten_file_perms(_p: &Path) {}

/// 文本去重指纹（daemon 轮询短路复用）。
///
/// blake3：算法规范跨编译器/进程/机器版本稳定（DefaultHasher 无此保证，
/// 存量库已在 v3→v4 迁移中一次性重算，见 migrate.rs 与 ADR-003）；
/// 256 位密码学强度，作为长期数据指纹无碰撞面担忧，热路径吞吐 GB/s 级
/// （与 max_clip_bytes 限额下的读取成本同量级，非瓶颈）。
/// 图片不走此函数：`img:` 前缀的 FNV 指纹本就稳定（见 image_content_key）。
pub fn hash_text(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
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

/// 去重写入的**共用骨架**（文本/图片两条路径共用，任务 2.6 / C6）：
/// `BEGIN IMMEDIATE` → 按 hash 查重 → 已存在则刷新 `ts`、否则 INSERT →
/// 提交 → 刷新当前项指针。
///
/// `BEGIN IMMEDIATE` 的必要性见 `insert_with` 的注释（多进程并发下
/// "先查后插"必须原子化）。`on_new_row` 在**事务内**对新插入的行执行额外工作
/// （图片路径写数据文件 + UPDATE image_path），其返回值原样回传给调用方；
/// 去重命中时回调不执行。
///
/// 返回 `(是否新插入, on_new_row 的返回值)`。
fn upsert_clip<T, F>(
    conn: &mut Connection,
    hash: &str,
    text: &str,
    mime: &str,
    ts: i64,
    size: i64,
    on_new_row: F,
) -> Result<(bool, Option<T>)>
where
    F: FnOnce(&rusqlite::Transaction<'_>, i64) -> Result<T>,
{
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
        touch_current(hash);
        return Ok((false, None));
    }
    tx.execute(
        "INSERT INTO clips (hash, text, mime, ts, size) VALUES (?1,?2,?3,?4,?5)",
        params![hash, text, mime, ts, size],
    )?;
    let id = tx.last_insert_rowid();
    let extra = on_new_row(&tx, id)?;
    tx.commit()?;
    touch_current(hash);
    Ok((true, Some(extra)))
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

    // 检查 + 插入的原子性由 upsert_clip 的 BEGIN IMMEDIATE 保证：否则多进程
    // 并发时（典型：fzf 选中旧条目 -> wl-copy 写回 -> daemon 捕获同一 hash）
    // 双双通过检查后一方撞 UNIQUE 报错并被静默吞掉
    let (inserted, _) = upsert_clip(&mut conn, &hash, &text, &mime, ts, size, |_, _| Ok(()))?;

    if inserted {
        enforce_max_items(&conn, cfg.max_items)?;
    }
    Ok(inserted)
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

    let dir = Config::images_dir();
    std::fs::create_dir_all(&dir)?;
    tighten_dir_perms(&dir);

    // 骨架（去重/提交/刷指针）与文本路径共用；差异只在 `on_new_row`——
    // 数据文件写入必须落在**同一事务窗口内**：先落 `.tmp-` 再原子 rename 到最终
    // 路径，任何一步失败即随 tx 回滚行，不再出现"有行无图"的永久残缺状态
    // （旧行为：先 commit 行、后写文件，中途崩溃则 hash 已占用，该图永远无法重录）
    let (inserted, extra) = upsert_clip(
        &mut conn,
        &hash,
        &placeholder,
        mime,
        ts,
        bytes.len() as i64,
        |tx, id| -> Result<InsertedImage> {
            let path = dir.join(format!("{id}.bin"));
            let tmp = dir.join(format!(".tmp-{id}.bin"));
            std::fs::write(&tmp, bytes)
                .with_context(|| format!("write image cache {}", tmp.display()))?;
            std::fs::rename(&tmp, &path)
                .with_context(|| format!("publish image cache {}", path.display()))?;
            tighten_file_perms(&path);
            tx.execute(
                "UPDATE clips SET image_path=?1 WHERE id=?2",
                params![path.to_string_lossy(), id],
            )?;
            Ok(InsertedImage { id, path })
        },
    )?;

    if inserted {
        enforce_max_items(&conn, cfg.max_items)?;
    }
    Ok(extra)
}

/// 执行 `DELETE ... RETURNING image_path` 并返回被删行的数据文件路径
/// （任务 2.6 / C7）。
///
/// 只收集路径、**不删文件**：删除时机由调用方决定——`prune_before` 必须在
/// commit 之后删（事务回滚时行还在，文件不能先没），而 `enforce_max_items`
/// 处于自动提交语义下可立即删。此前两处各写一遍同样的收尸逻辑。
///
/// 注：`gc_images` 不适用本助手——它需要按行累计字节数逐条淘汰，是**有意**
/// 的逐行删除（见其注释），与这里"一条 DELETE 批量删"的形态不同。
fn delete_rows_image_paths(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params, |r| r.get::<_, Option<String>>(0))?;
    let mut out = Vec::new();
    for p in rows {
        if let Some(p) = p? {
            out.push(p);
        }
    }
    Ok(out)
}

/// 上限裁剪（文本/图片共用）。pub(crate)：import（backup.rs）结束后按当前
/// 配置执行一次，与捕获路径同一上限语义。
///
/// **对当前项的保护是隐式的**：这里只排除 `pinned`，不显式排除 `current` 指针
/// 指向的行（写法与 `gc_images`/`prune_before` 不同）。这不是遗漏——当前项指针
/// 只在捕获成功时刷新，而捕获同时会刷新该行 `ts`（新入库 INSERT，或去重时
/// UPDATE ts），故当前项恒为最新 ts，不满足"按 ts 最旧优先淘汰"的前提。
/// 该不变式由 `max_items_eviction_never_drops_the_current_item` 单测锁定；
/// 若将来把"指针刷新"与"ts 更新"解耦，必须先把这里改成显式排除当前项。
pub(crate) fn enforce_max_items(conn: &Connection, max_items: usize) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;
    if count > max_items as i64 {
        let to_del = count - max_items as i64;
        // RETURNING 带出被淘汰条目的数据文件路径，行删文件也删（否则
        // images/ 只进不出，截图类负载下 state 目录会无限膨胀）
        let paths = delete_rows_image_paths(
            conn,
            "DELETE FROM clips WHERE id IN (SELECT id FROM clips WHERE pinned=0 ORDER BY ts ASC LIMIT ?1) RETURNING image_path",
            &[&to_del],
        )?;
        for p in paths {
            let _ = std::fs::remove_file(p);
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

// =====================================================================
// v0.6（任务 2.3）：数据统计与维护（stats / vacuum / prune）
//
// 验收标准是"用户可自助管理磁盘占用"。两个磁盘口径：
// - 库体积 = db.sqlite + -wal。WAL 是未检查点的数据载荷，要计；
//   -shm 是固定 32 KiB 的瞬态共享内存映射，不是数据，不计
// - 图片体积 = images/ 目录磁盘实测。DB 内 size 列是入库时刻的字节数，
//   与磁盘真值之间隔着孤儿文件与失败的行删清理，统计以实测为准
// =====================================================================

fn db_disk_bytes() -> u64 {
    let p = Config::db_path();
    // 只计主文件与 WAL；-shm 是固定 32 KiB 的瞬态共享内存映射，非数据载荷
    // ——vacuum 若在连接存活时测量，shm 会把"后"体积顶大 32 KiB 造成假增长
    [p.clone(), p.with_extension("sqlite-wal")]
        .iter()
        .filter_map(|f| std::fs::metadata(f).ok())
        .map(|m| m.len())
        .sum()
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct StoreStats {
    pub total: i64,
    pub pinned: i64,
    pub image_entries: i64,
    pub db_bytes: u64,
    pub images_disk_bytes: u64,
    pub oldest_ts: Option<i64>,
    pub newest_ts: Option<i64>,
}

pub fn stats() -> Result<StoreStats> {
    let conn = connect()?;
    let one = |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, [], |r| r.get(0))?) };
    let total = one("SELECT COUNT(*) FROM clips")?;
    let pinned = one("SELECT COUNT(*) FROM clips WHERE pinned=1")?;
    let image_entries = one("SELECT COUNT(*) FROM clips WHERE image_path IS NOT NULL")?;
    let (oldest_ts, newest_ts) = conn.query_row("SELECT MIN(ts), MAX(ts) FROM clips", [], |r| {
        Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;
    Ok(StoreStats {
        total,
        pinned,
        image_entries,
        db_bytes: db_disk_bytes(),
        images_disk_bytes: dir_size(&Config::images_dir()),
        oldest_ts,
        newest_ts,
    })
}

/// VACUUM 重建库文件并截断 WAL，返回 (前, 后) 磁盘字节。
/// daemon 常驻连接下若有并发写事务在途，VACUUM 会在 busy_timeout（5s）
/// 内等待而非令 daemon 中断；超时报错即可重试。
pub fn vacuum() -> Result<(u64, u64)> {
    let before = db_disk_bytes();
    let after = {
        let conn = connect()?;
        conn.execute_batch("VACUUM")?;
        // VACUUM 本身会写新页进 WAL，不截断的话"后"口径虚高；
        // 连接须先关闭再测量（关闭时残余 WAL 自动检查点清除）
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        drop(conn);
        db_disk_bytes()
    };
    Ok((before, after))
}

#[derive(Debug)]
pub struct PruneOutcome {
    pub deleted: usize,
    pub images_deleted: usize,
    /// 回收载荷字节（文本 = 文本字节；图片 = 数据文件字节，即 size 列）。
    /// 库文件体积的回落要等 VACUUM，这里只是载荷口径
    pub freed_bytes: i64,
}

/// prune（任务 2.3）：删除 ts < cutoff_ms 的旧条目。保护语义与图片 GC
/// （1.3）一致：星标不删；当前项（state/current 指针 ≈ Ctrl+V 会粘出的
/// 内容）不删——删了用户粘贴会落空。dry_run 只统计，不动任何数据。
/// 行删经 FTS 触发器同步索引；图片数据文件随行删除，清理失败不阻断
/// （残留由 prune_orphan_images 兜底，同 delete/enforce_max_items 口径）。
pub fn prune_before(cutoff_ms: i64, dry_run: bool) -> Result<PruneOutcome> {
    let mut conn = connect()?;
    let cur_hash = current_hash().unwrap_or_default();
    let guard = "ts < ?1 AND pinned=0 AND hash != ?2";
    if dry_run {
        let (n, img, bytes) = conn.query_row(
            &format!(
                "SELECT COUNT(*), COUNT(image_path), COALESCE(SUM(size),0)
                 FROM clips WHERE {guard}"
            ),
            params![cutoff_ms, cur_hash],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        )?;
        return Ok(PruneOutcome {
            deleted: n as usize,
            images_deleted: img as usize,
            freed_bytes: bytes,
        });
    }
    // BEGIN IMMEDIATE：SUM 与 DELETE 之间 daemon 可能并发写入，
    // 不加事务的话 freed_bytes 与实际删除行数会互相失真
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let freed: i64 = tx.query_row(
        &format!("SELECT COALESCE(SUM(size),0) FROM clips WHERE {guard}"),
        params![cutoff_ms, cur_hash],
        |r| r.get(0),
    )?;
    let deleted: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM clips WHERE {guard}"),
        params![cutoff_ms, cur_hash],
        |r| r.get(0),
    )?;
    // 同一事务内完成统计与删除（C7 助手只收集路径，删文件见下）
    let ps: [&dyn rusqlite::ToSql; 2] = [&cutoff_ms, &cur_hash];
    let image_files = delete_rows_image_paths(
        &tx,
        &format!(
            "DELETE FROM clips WHERE id IN (SELECT id FROM clips WHERE {guard})
             RETURNING image_path"
        ),
        &ps,
    )?;
    tx.commit()?;
    // 文件删除放在 commit 之后：commit 失败回滚时行还在，文件不能先没
    // （prune_orphan_images 只兜底"有文件无行"，反向残缺无清扫）。commit 后
    // 删除失败的残留与既有口径一致，同样由孤儿清扫兜底
    let images_deleted = image_files.len();
    for p in image_files {
        let _ = std::fs::remove_file(p);
    }
    Ok(PruneOutcome {
        deleted: deleted as usize,
        images_deleted,
        freed_bytes: freed,
    })
}

/// `YYYY-MM-DD` → 本地时区当日零点的毫秒时间戳（prune --before 的参数口径）。
/// 按"某天之前"的用户语义应以本地日历日切分，而非 UTC——北京时间 8 月 1 日
/// 零点 = UTC 前一日 16:00，按 UTC 会多删少删 8 小时内的条目。
pub fn parse_local_date_ms(s: &str) -> Result<i64> {
    use chrono::TimeZone;
    let d = chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
        .with_context(|| format!("日期格式应为 YYYY-MM-DD：{s}"))?;
    let dt = d.and_hms_opt(0, 0, 0).expect("零点恒为合法时刻");
    match chrono::Local.from_local_datetime(&dt).single() {
        Some(t) => Ok(t.timestamp_millis()),
        None => anyhow::bail!("本地时区在该日期零点存在歧义（DST 切换）：{s}"),
    }
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
                // 用 cliphist decode 拿全量文本。id 走 stdin 管道而非
                // `sh -c "echo {id} | cliphist decode"`——去掉一层 shell 解析，
                // 免去字符串拼接（任务 2.6 / D8）
                let decoded = std::process::Command::new("cliphist")
                    .arg("decode")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .ok()
                    .and_then(|mut c| {
                        c.stdin
                            .as_mut()?
                            .write_all(format!("{id}\n").as_bytes())
                            .ok()?;
                        c.wait_with_output().ok()
                    })
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
mod tests;
