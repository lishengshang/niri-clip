use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config::Config;

// v0.3: TUI 懒加载 + 缓存
pub const TUI_LIMIT: usize = 300;
const CACHE_TTL: Duration = Duration::from_millis(200);
static MENU_CACHE: std::sync::OnceLock<Mutex<CachedList>> = std::sync::OnceLock::new();

struct CachedList {
    clips: Vec<Clip>,
    at: Instant,
    limit: usize,
}

fn cache() -> &'static Mutex<CachedList> {
    MENU_CACHE.get_or_init(|| {
        Mutex::new(CachedList {
            clips: Vec::new(),
            at: Instant::now() - CACHE_TTL * 2,
            limit: 0,
        })
    })
}

#[derive(Debug, Clone)]
pub struct Clip {
    pub id: i64,
    pub hash: String,
    pub text: String,
    pub mime: String,
    pub ts: i64,
    pub pinned: bool,
}

fn connect() -> Result<Connection> {
    let path = Config::db_path();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let conn = Connection::open(&path).context("open sqlite")?;
    conn.execute_batch(
        "
        PRAGMA journal_mode=WAL;
        PRAGMA synchronous=NORMAL;
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
        ",
    )?;
    // FTS5 虚拟表用于 v1.0 全文搜索，v0.2 先创建占位
    let _ = conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(text, content='clips', content_rowid='id');",
    );
    Ok(conn)
}

fn hash_text(s: &str) -> String {
    // 简单 blake3 替代：用 std hash + 长度，避免额外依赖
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}-{}", h.finish(), s.len())
}

pub fn should_ignore(text: &str, cfg: &Config) -> bool {
    if text.chars().count() < cfg.min_store_length {
        return true;
    }
    if let Ok(re) = Regex::new(&cfg.ignore_regex) {
        if re.is_match(text) {
            return true;
        }
    }
    false
}

pub fn insert(text: String, mime: Option<String>) -> Result<bool> {
    let cfg = Config::load();
    if should_ignore(&text, &cfg) {
        return Ok(false);
    }
    let conn = connect()?;
    let hash = hash_text(&text);
    let ts = Utc::now().timestamp_millis();
    let mime = mime.unwrap_or_else(|| "text/plain".to_string());
    let size = text.len() as i64;

    // 去重：hash 已存在则更新 ts 并返回 false (不新增)
    let exists: Option<i64> = conn
        .query_row("SELECT id FROM clips WHERE hash=?1", params![hash], |r| r.get(0))
        .ok();
    if let Some(id) = exists {
        conn.execute("UPDATE clips SET ts=?1 WHERE id=?2", params![ts, id])?;
        return Ok(false);
    }

    conn.execute(
        "INSERT INTO clips (hash, text, mime, ts, size) VALUES (?1,?2,?3,?4,?5)",
        params![hash, text, mime, ts, size],
    )?;

    // FTS 同步
    let _ = conn.execute(
        "INSERT INTO clips_fts(rowid, text) VALUES (last_insert_rowid(), ?1)",
        params![text],
    );
    invalidate_cache();

    // 超量清理
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;
    if count > cfg.max_items as i64 {
        let to_del = count - cfg.max_items as i64;
        conn.execute(
            "DELETE FROM clips WHERE id IN (SELECT id FROM clips WHERE pinned=0 ORDER BY ts ASC LIMIT ?1)",
            params![to_del],
        )?;
    }
    Ok(true)
}

pub fn list(limit: usize) -> Result<Vec<Clip>> {
    let cfg = Config::load();
    let conn = connect()?;
    let order = if cfg.pinned_on_top {
        "pinned DESC, ts DESC, id DESC"
    } else {
        "ts DESC, id DESC"
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, hash, text, mime, ts, pinned FROM clips ORDER BY {} LIMIT ?1",
        order
    ))?;
    let rows = stmt.query_map(params![limit as i64], |r| {
        Ok(Clip {
            id: r.get(0)?,
            hash: r.get(1)?,
            text: r.get(2)?,
            mime: r.get(3)?,
            ts: r.get(4)?,
            pinned: r.get::<_, i64>(5)? != 0,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// v0.3: TUI 专用 300 条懒加载 + 200ms 缓存
pub fn list_tui() -> Result<Vec<Clip>> {
    let mut c = cache().lock().unwrap();
    if c.limit == TUI_LIMIT && c.at.elapsed() < CACHE_TTL && !c.clips.is_empty() {
        return Ok(c.clips.clone());
    }
    let clips = list(TUI_LIMIT)?;
    c.clips = clips.clone();
    c.at = Instant::now();
    c.limit = TUI_LIMIT;
    Ok(clips)
}

pub fn invalidate_cache() {
    if let Some(m) = MENU_CACHE.get() {
        if let Ok(mut c) = m.lock() {
            c.at = Instant::now() - CACHE_TTL * 2;
        }
    }
}

/// 10k 压测：插入 10k 条并测量 list 耗时
pub fn bench_10k() -> Result<Duration> {
    let start = Instant::now();
    let clips = list(10000)?;
    let elapsed = start.elapsed();
    println!("[bench] list 10k: {} items in {:?} ({:.2} items/ms)", clips.len(), elapsed, clips.len() as f64 / elapsed.as_millis().max(1) as f64);
    if elapsed.as_millis() > 50 {
        eprintln!("[bench] WARN: >50ms, consider VACUUM or index");
    }
    Ok(elapsed)
}

pub fn delete(id: i64) -> Result<()> {
    let conn = connect()?;
    conn.execute("DELETE FROM clips WHERE id=?1", params![id])?;
    let _ = conn.execute("DELETE FROM clips_fts WHERE rowid=?1", params![id]);
    invalidate_cache();
    Ok(())
}

pub fn wipe() -> Result<()> {
    let conn = connect()?;
    conn.execute("DELETE FROM clips", [])?;
    conn.execute("DELETE FROM clips_fts", [])?;
    invalidate_cache();
    // v1.0 独立：不再操作 cliphist，旧数据请手动 cliphist wipe
    Ok(())
}

pub fn toggle_pin(id: i64) -> Result<bool> {
    let conn = connect()?;
    let cur: i64 = conn.query_row("SELECT pinned FROM clips WHERE id=?1", params![id], |r| r.get(0))?;
    let new = if cur == 0 { 1 } else { 0 };
    conn.execute("UPDATE clips SET pinned=?1 WHERE id=?2", params![new, id])?;
    invalidate_cache();
    Ok(new == 1)
}

pub fn is_pinned(id: i64) -> Result<bool> {
    let conn = connect()?;
    let v: i64 = conn.query_row("SELECT pinned FROM clips WHERE id=?1", params![id], |r| r.get(0))?;
    Ok(v != 0)
}

pub fn get(id: i64) -> Result<Clip> {
    let conn = connect()?;
    let c = conn.query_row(
        "SELECT id, hash, text, mime, ts, pinned FROM clips WHERE id=?1",
        params![id],
        |r| {
            Ok(Clip {
                id: r.get(0)?,
                hash: r.get(1)?,
                text: r.get(2)?,
                mime: r.get(3)?,
                ts: r.get(4)?,
                pinned: r.get::<_, i64>(5)? != 0,
            })
        },
    )?;
    Ok(c)
}

pub fn db_path() -> PathBuf {
    Config::db_path()
}

/// 从 cliphist 迁移（一次性）
pub fn migrate_from_cliphist() -> Result<usize> {
    let out = std::process::Command::new("cliphist").arg("list").output();
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return Ok(0),
    };
    let s = String::from_utf8_lossy(&out.stdout);
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
                if insert(decoded, None)? {
                    n += 1;
                }
            }
        }
    }
    Ok(n)
}
