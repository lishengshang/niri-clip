//! schema 演进与旧库搬迁（自 store.rs 拆出，纯代码搬移）。
//!
//! - `migrate_legacy_db`：v0.4 的 ~/.cache → ~/.local/state 一次性搬迁
//! - `migrate_schema`：`PRAGMA user_version` 驱动的版本化迁移
//!
//! 变更 schema 必须在此新增版本号与迁移步骤（禁止原地改旧步骤）。

use anyhow::{anyhow, Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::store::{hash_text, tighten_dir_perms, tighten_file_perms};

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
/// 版本 3 -> 4（任务 2.2）：文本 hash 统一为 blake3（见 migrate_blake3）
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
    if ver < 4 {
        migrate_blake3(conn)?;
    }
    Ok(())
}

/// 任务 2.2（v3→v4）：文本 hash 统一为 blake3（ADR-003）。
///
/// DefaultHasher 算法跨编译器/进程不稳定（rustc 升级即变），存量库可能
/// 已含"同文本不同 hash"行——它们在旧 UNIQUE 下共存，换成稳定算法后
/// 映射到同一指纹，若不合并直接重建 UNIQUE 会报错，若先删后插则会翻倍。
///
/// 步骤（快照在事务外：VACUUM INTO 不允许在事务内执行）：
/// 1. `VACUUM INTO` 快照到 `state/db.sqlite.pre-blake3`（覆盖旧份）；
///    快照失败直接放弃迁移——宁可继续旧 hash，不做无退路的全表手术
/// 2. BEGIN IMMEDIATE 事务内重读 user_version（防双进程竞态迁移）
/// 3. 文本行（hash 非 `img:` 前缀；NULL hash 一并修复）按 blake3(text)
///    分组：幸存行 = ts 最大（并列取 id 大），pinned 取 OR，image_path
///    为空则继承被并行的；UPDATE 幸存行后 DELETE 其余——DELETE 触发
///    clips_fts_ad，FTS 自动同步，无需重建索引（幸存行不动 text，
///    不触发 clips_fts_au）
/// 4. PRAGMA user_version=4，提交
/// 5. 事务外重映射 state/current 指针（best-effort，写失败不阻断）
fn migrate_blake3(conn: &Connection) -> Result<()> {
    let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ver >= 4 {
        return Ok(());
    }

    let text_rows: i64 = conn.query_row(
        "SELECT COUNT(*) FROM clips WHERE hash IS NULL OR hash NOT LIKE 'img:%'",
        [],
        |r| r.get(0),
    )?;
    if text_rows > 0 {
        snapshot_before_blake3(conn)?;
    }

    // BEGIN IMMEDIATE 手动起事务：migrate_schema 持 &Connection（connect 签名
    // 约束），rusqlite 的借用事务（unchecked_transaction）只有 DEFERRED 行为，
    // 抢占式写锁需手动 BEGIN。版本重读/全表重算/回填版本必须在同一写锁窗口
    // 内原子完成；COMMIT 前须 finalize 所有语句（语句句柄均已 drop）
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let (remap, merged) = match migrate_blake3_tx(conn) {
        Ok(v) => conn
            .execute_batch("COMMIT")
            .map_err(anyhow::Error::from)
            .map(|_| v)?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    };

    // ▶ 指针存的是旧 hash，不重映射则"最后一次复制"标记静默失效
    if let Some(cur) = crate::store::current_hash() {
        if let Some(new) = remap.get(&cur) {
            crate::store::touch_current(new);
        }
    }

    eprintln!(
        "[niri-clip] blake3 迁移完成：{merged} 条重复已合并（快照： {}）",
        blake3_snapshot_path().display()
    );
    Ok(())
}

/// 事务体：成功返回（旧 hash → 新 hash 映射，合并数），由调用方 COMMIT
fn migrate_blake3_tx(conn: &Connection) -> Result<(HashMap<String, String>, usize)> {
    // 事务内重读版本：另一进程可能已在我们快照期间完成迁移
    let ver: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if ver >= 4 {
        return Ok((HashMap::new(), 0));
    }

    // 全量载入文本行的指纹与元数据（不驻留正文：扫描时即算 blake3，
    // 内存 = 行数 × ~150B 元数据）；MAX_ITEMS/max_clip_bytes 限界下可控，
    // 100k 行极端规模的压力验证归任务 2.5
    struct TextRow {
        id: i64,
        hash: Option<String>,
        b3: String,
        ts: i64,
        pinned: bool,
        image_path: Option<String>,
    }
    let mut stmt = conn.prepare(
        "SELECT id, hash, text, ts, COALESCE(pinned, 0), image_path FROM clips
         WHERE hash IS NULL OR hash NOT LIKE 'img:%'",
    )?;
    let rows: Vec<TextRow> = stmt
        .query_map([], |r| {
            let text: String = r.get(2)?;
            Ok(TextRow {
                id: r.get(0)?,
                hash: r.get(1)?,
                b3: hash_text(&text),
                ts: r.get(3)?,
                pinned: r.get::<_, i64>(4)? != 0,
                image_path: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
    drop(stmt);

    let mut groups: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        groups.entry(&row.b3).or_default().push(i);
    }

    // 幸存行旧 hash → 新 hash 的映射，供迁移后重映射当前项指针。
    // UPDATE 不会撞 UNIQUE：幸存行间 b3 互异，被并行仍持旧 hash（格式不同）
    let mut remap: HashMap<String, String> = HashMap::new();
    let mut merged: usize = 0;
    for (b3, idxs) in &groups {
        let &surv = idxs
            .iter()
            .max_by_key(|&&i| (rows[i].ts, rows[i].id))
            .unwrap();
        // 星标取 OR：任一重复行被星标，合并后不应丢星；
        // image_path 本不应出现在文本行，防御性继承兜底
        let pinned = idxs.iter().any(|&i| rows[i].pinned);
        let image_path = rows[surv].image_path.clone().or_else(|| {
            idxs.iter()
                .filter_map(|&i| rows[i].image_path.clone())
                .next()
        });
        if let Some(h) = &rows[surv].hash {
            remap.insert(h.clone(), (*b3).to_string());
        }
        conn.execute(
            "UPDATE clips SET hash=?1, pinned=?2, image_path=?3 WHERE id=?4",
            rusqlite::params![b3, pinned as i64, image_path, rows[surv].id],
        )?;
        for &i in idxs.iter().filter(|&&i| i != surv) {
            if let Some(h) = &rows[i].hash {
                remap.insert(h.clone(), (*b3).to_string());
            }
            conn.execute(
                "DELETE FROM clips WHERE id=?1",
                rusqlite::params![rows[i].id],
            )?;
            merged += 1;
        }
    }

    conn.execute_batch("PRAGMA user_version=4;")?;
    Ok((remap, merged))
}

/// 迁移前快照路径（固定名，每次迁移前覆盖；用户可手动删除）
fn blake3_snapshot_path() -> PathBuf {
    Config::state_dir().join("db.sqlite.pre-blake3")
}

fn snapshot_before_blake3(conn: &Connection) -> Result<()> {
    let snap = blake3_snapshot_path();
    if let Some(p) = snap.parent() {
        std::fs::create_dir_all(p)?;
        tighten_dir_perms(p);
    }
    // VACUUM INTO 要求目标不存在：先清掉上一次的快照
    let _ = std::fs::remove_file(&snap);
    conn.execute("VACUUM INTO ?1", [snap.to_string_lossy().as_ref()])
        .with_context(|| format!("snapshot db to {}", snap.display()))?;
    tighten_file_perms(&snap);
    Ok(())
}
