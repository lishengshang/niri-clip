//! schema 演进与旧库搬迁（自 store.rs 拆出，纯代码搬移）。
//!
//! - `migrate_legacy_db`：v0.4 的 ~/.cache → ~/.local/state 一次性搬迁
//! - `migrate_schema`：`PRAGMA user_version` 驱动的版本化迁移
//!
//! 变更 schema 必须在此新增版本号与迁移步骤（禁止原地改旧步骤）。

use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

use crate::config::Config;
use crate::store::tighten_dir_perms;

/// v0.4：数据库从 ~/.cache 迁往 ~/.local/state（XDG state 规范——
/// 剪贴板历史属于应持久的状态数据，放在 ~/.cache 会被系统清理工具误删）。
/// 首次发现旧库且新库缺失时，用 `VACUUM INTO` 做一致性快照搬迁，
/// 旧库保留作为备份不动。
pub(crate) fn migrate_legacy_db(new_path: &Path) -> Result<()> {
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
/// 版本 2 -> 3（任务 2.1）：clips_fts 全文索引（FTS5 trigram，中英文子串
/// 均命中，选型见 ADR-002）。外部内容表（content='clips'）不占双倍存储，
/// 由三触发器与 clips 行同步；存量行迁移时一次性回填。旧库升级无损：
/// 只增表/触发器，不动 clips 行
pub(crate) fn migrate_schema(conn: &Connection) -> Result<()> {
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
    if ver < 3 {
        conn.execute_batch(
            "
            CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts USING fts5(
                text, content='clips', content_rowid='id', tokenize='trigram'
            );
            INSERT INTO clips_fts(rowid, text) SELECT id, text FROM clips;
            CREATE TRIGGER IF NOT EXISTS clips_fts_ai AFTER INSERT ON clips BEGIN
                INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
            END;
            CREATE TRIGGER IF NOT EXISTS clips_fts_ad AFTER DELETE ON clips BEGIN
                INSERT INTO clips_fts(clips_fts, rowid, text)
                VALUES ('delete', old.id, old.text);
            END;
            CREATE TRIGGER IF NOT EXISTS clips_fts_au AFTER UPDATE OF text ON clips BEGIN
                INSERT INTO clips_fts(clips_fts, rowid, text)
                VALUES ('delete', old.id, old.text);
                INSERT INTO clips_fts(rowid, text) VALUES (new.id, new.text);
            END;
            ",
        )?;
        conn.execute_batch("PRAGMA user_version=3;")?;
    }
    Ok(())
}
