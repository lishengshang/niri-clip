//! 任务 2.4：历史导出与回灌（`export --json` / `import`）。
//!
//! 格式 v1（NDJSON，选型与被拒备选见 ADR-004）：首行 header 元数据，
//! 随后每行一个条目。读写全程流式（按行），内存峰值 = 最大单条目，
//! 大图片库不爆内存；同库两次导出字节稳定（ts ASC, id ASC，可 diff）。
//!
//! hash 是幂等合并键：文本 = blake3(text)（2.2 起跨编译器/机器稳定）、
//! 图片 = `img:` FNV key。该格式即 Phase 5「历史内容动作插件化」的
//! 扩展点，一旦发布只增不改（header.version 供未来演进）。
//!
//! 安全口径：备份是全量剪贴板明文，导出文件权限收紧 0600；import 侧
//! 对每条重算 hash 做完整性校验，损坏条目跳过并警告，好条目照常入库。

use anyhow::{Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chrono::Utc;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;
use crate::store::{
    hash_text, image_content_key, remove_image_files, trim_history, write_image_payload,
};

/// 格式标识（header 行）。不识别即拒绝；version 高于本实现则提示升级
pub const EXPORT_FORMAT: &str = "niri-clip-export";
pub const EXPORT_VERSION: u32 = 1;

#[derive(Debug)]
pub struct ExportOutcome {
    pub count: usize,
    /// 图片条目数与其数据文件字节数（base64 内嵌前）
    pub images: usize,
    pub image_bytes: u64,
}

#[derive(Debug, Default)]
pub struct ImportOutcome {
    /// 新入库条数（dry-run 下语义为"将导入"）
    pub imported: usize,
    /// hash 已存在而跳过（不刷新 ts——import 不是捕获，不打扰 ▶ 时序）
    pub exists: usize,
    /// JSON 解析失败 / 完整性校验失败而跳过
    pub invalid: usize,
}

#[derive(Serialize)]
struct ExportHeader {
    format: &'static str,
    version: u32,
    /// 导出时的 schema 版本，仅供人工/工具核对，import 不依赖
    /// （schema 演进由 connect() 的 user_version 迁移链负责）
    user_version: i64,
    exported_at: i64,
    count: usize,
}

#[derive(Serialize, Deserialize)]
struct ExportEntry {
    hash: String,
    text: String,
    mime: String,
    ts: i64,
    pinned: bool,
    /// 入库时刻的字节口径（文本 = text 字节；图片 = 数据文件字节）。
    /// import 侧重算真值，此字段仅供外部工具与人工核对
    #[serde(default)]
    size: i64,
    /// 仅图片条目：数据文件字节内嵌。image_path 是本机绝对路径，不导出
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image_base64: Option<String>,
}

pub(crate) struct BackupFile(PathBuf);

impl BackupFile {
    pub(crate) fn create(destination: &Path) -> Result<(Self, File)> {
        match destination.symlink_metadata() {
            Ok(_) => anyhow::bail!("备份目标已存在，不做覆盖：{}", destination.display()),
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(e).with_context(|| format!("检查 {}", destination.display())),
        }
        Self::temporary(destination)
    }

    pub(crate) fn temporary(destination: &Path) -> Result<(Self, File)> {
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;

        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        loop {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!(".niri-clip-backup-{}-{sequence}", std::process::id());
            let temporary = parent.join(name);
            if temporary.file_name() == destination.file_name() {
                continue;
            }
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)
            {
                Ok(file) => {
                    let backup = Self(temporary);
                    if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
                        file.set_permissions(fs::Permissions::from_mode(0o600))
                            .context("备份位置不支持 0600 文件权限")?;
                        anyhow::ensure!(
                            file.metadata()?.permissions().mode() & 0o777 == 0o600,
                            "备份位置不支持 0600 文件权限"
                        );
                    }
                    return Ok((backup, file));
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e).with_context(|| format!("创建 {}", temporary.display())),
            }
        }
    }

    pub(crate) fn vacuum_from(&self, conn: &Connection) -> Result<()> {
        let path = std::path::absolute(&self.0)?;
        let name = path.to_str().context("SQLite 备份路径必须是 UTF-8")?;
        conn.execute("VACUUM INTO ?1", [name])
            .with_context(|| format!("VACUUM INTO {}", self.0.display()))?;
        Ok(())
    }

    pub(crate) fn publish(self, destination: &Path) -> Result<()> {
        File::open(&self.0)?.sync_all()?;
        // Refuse an existing database, backup, or link created during the export.
        publish_new_file(&self.0, destination)
            .with_context(|| format!("写入备份 {}", destination.display()))
    }

    /// The migration owns a fixed snapshot name. Preserve the previous copy until
    /// SQLite has finished writing its replacement.
    pub(crate) fn replace(self, destination: &Path) -> Result<()> {
        File::open(&self.0)?.sync_all()?;
        fs::rename(&self.0, destination)
            .with_context(|| format!("替换迁移快照 {}", destination.display()))
    }
}

fn publish_new_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let from = CString::new(source.as_os_str().as_bytes())?;
        let to = CString::new(destination.as_os_str().as_bytes())?;
        // SAFETY: both path strings are NUL-terminated and valid for the call.
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP)
        ) {
            return Err(error);
        }
    }
    fs::hard_link(source, destination)
}

impl Drop for BackupFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// 全量导出为 NDJSON。`out = None` 写 stdout（可管道 `| gzip`），
/// `Some(path)` 写文件（0600——内容是全量剪贴板明文）。全表导出不受
/// max_items / MENU_LIMIT 限制。
pub fn export_json_file(out: Option<&Path>) -> Result<ExportOutcome> {
    let backup = out.map(BackupFile::create).transpose()?;
    let (temporary, writer): (Option<BackupFile>, Box<dyn Write>) = match backup {
        Some((temporary, file)) => (Some(temporary), Box::new(file)),
        None => (None, Box::new(std::io::stdout().lock())),
    };
    let mut conn = crate::store::connect()?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    // deferred 读事务：COUNT 与全表扫描取同一快照，daemon 并发写不会让
    // header.count 与实际行数互相失真（--sqlite 快照不在本事务内）
    let tx = conn.transaction()?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;

    let mut w = BufWriter::new(writer);

    serde_json::to_writer(
        &mut w,
        &ExportHeader {
            format: EXPORT_FORMAT,
            version: EXPORT_VERSION,
            user_version,
            exported_at: Utc::now().timestamp_millis(),
            count: count as usize,
        },
    )?;
    w.write_all(b"\n")?;

    let mut stmt = tx.prepare(
        "SELECT hash, text, mime, ts, COALESCE(pinned,0), COALESCE(size,0), image_path
         FROM clips ORDER BY ts ASC, id ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, Option<String>>(6)?,
        ))
    })?;

    let mut n = 0usize;
    let mut images = 0usize;
    let mut image_bytes = 0u64;
    for row in rows {
        let (hash, text, mime, ts, pinned, size, image_path) = row?;
        // 图片载荷随行读出内嵌。数据文件缺失（异常删除等）不中断整次导出：
        // 记警告并省略 image_base64，import 侧会因载荷缺失计为 invalid
        let image_base64 = match &image_path {
            Some(p) => match std::fs::read(p) {
                Ok(bytes) => {
                    images += 1;
                    image_bytes += bytes.len() as u64;
                    Some(BASE64.encode(bytes))
                }
                Err(e) => {
                    eprintln!(
                        "[niri-clip export] 图片数据文件缺失 {}：{e}（该条目回灌时将无载荷）",
                        p
                    );
                    None
                }
            },
            None => None,
        };
        let entry = ExportEntry {
            hash,
            text,
            mime,
            ts,
            pinned: pinned != 0,
            size,
            image_base64,
        };
        serde_json::to_writer(&mut w, &entry)?;
        w.write_all(b"\n")?;
        n += 1;
    }
    w.flush()?;
    drop(stmt);
    tx.commit()?; // 释放读快照
    if let (Some(temporary), Some(destination)) = (temporary, out) {
        temporary.publish(destination)?;
    }
    Ok(ExportOutcome {
        count: n,
        images,
        image_bytes,
    })
}

/// `--sqlite`：`VACUUM INTO` 物理快照——完整 db.sqlite 副本，同版本精确
/// 还原用，与 JSON 导出互补（JSON 跨版本/跨机器可携 + hash 幂等合并；
/// 物理快照保真但绑 schema 版本）。目标已存在直接报错（VACUUM INTO 要求
/// 目标不存在；备份命令绝不静默覆盖，重打快照请先手动删除旧份）
pub fn export_sqlite(path: &Path) -> Result<()> {
    let (temporary, file) = BackupFile::create(path)?;
    drop(file);
    let conn = crate::store::connect()?;
    temporary.vacuum_from(&conn)?;
    temporary.publish(path)
}

#[derive(Deserialize)]
struct ImportHeader {
    format: String,
    version: u32,
}

/// 校验一行并还原为可入库载荷。图片条目按 FNV key 重算比对；文本条目按
/// blake3 重算比对——损坏/被篡改的条目在这里被拦截
enum ValidEntry {
    Text {
        hash: String,
        text: String,
        mime: String,
        ts: i64,
        pinned: bool,
        size: i64,
    },
    Image {
        hash: String,
        mime: String,
        ts: i64,
        pinned: bool,
        bytes: Vec<u8>,
    },
}

fn parse_validate(line: &str) -> std::result::Result<ValidEntry, String> {
    let e: ExportEntry =
        serde_json::from_str(line).map_err(|err| format!("JSON 解析失败：{err}"))?;
    if e.hash.is_empty() {
        return Err("hash 为空".into());
    }
    match &e.image_base64 {
        Some(b64) => {
            let bytes = BASE64
                .decode(b64.trim().as_bytes())
                .map_err(|err| format!("image_base64 解码失败：{err}"))?;
            let key = image_content_key(&e.mime, &bytes);
            if key != e.hash {
                return Err(format!("图片 hash 不匹配：文件 {} vs 重算 {}", e.hash, key));
            }
            Ok(ValidEntry::Image {
                hash: e.hash,
                mime: e.mime,
                ts: e.ts,
                pinned: e.pinned,
                bytes,
            })
        }
        None => {
            if e.hash.starts_with("img:") {
                return Err("图片条目缺少 image_base64 载荷".into());
            }
            let h = hash_text(&e.text);
            if h != e.hash {
                return Err(format!("文本 hash 不匹配：文件 {} vs 重算 {}", e.hash, h));
            }
            let size = e.text.len() as i64;
            Ok(ValidEntry::Text {
                hash: e.hash,
                text: e.text,
                mime: e.mime,
                ts: e.ts,
                pinned: e.pinned,
                size,
            })
        }
    }
}

fn insert_entry(tx: &rusqlite::Transaction, e: ValidEntry) -> Result<()> {
    match e {
        ValidEntry::Text {
            hash,
            text,
            mime,
            ts,
            pinned,
            size,
        } => {
            // 自写 INSERT 保留原 ts/pinned（insert_with 强制 ts=now 且无 pinned）
            tx.execute(
                "INSERT INTO clips (hash, text, mime, ts, pinned, size) VALUES (?1,?2,?3,?4,?5,?6)",
                params![hash, text, mime, ts, pinned as i64, size],
            )?;
        }
        ValidEntry::Image {
            hash,
            mime,
            ts,
            pinned,
            bytes,
        } => {
            // text 由 mime+字节重生成 placeholder（与 insert_image_with 同构，幂等）
            let placeholder = format!("[image {mime} {} bytes]", bytes.len());
            tx.execute(
                "INSERT INTO clips (hash, text, mime, ts, pinned, size) VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    hash,
                    placeholder,
                    mime,
                    ts,
                    pinned as i64,
                    bytes.len() as i64
                ],
            )?;
            write_image_payload(tx, tx.last_insert_rowid(), &bytes)?;
        }
    }
    Ok(())
}

/// 从 NDJSON 备份回灌合并。hash 幂等（已存在跳过，不刷新 ts）；每条重算
/// hash 完整性校验，损坏跳过并警告。实际写入在单个 BEGIN IMMEDIATE 事务内
/// 原子提交；不触碰 ▶ 指针；结束按当前配置执行一次历史裁剪
/// （与捕获路径同一上限语义，pinned 保护自然沿用）。
/// dry_run 只校验 + 统计，不动任何数据（对齐 prune --dry-run 风格）。
pub fn import_file(path: &Path, dry_run: bool) -> Result<ImportOutcome> {
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut lines = BufReader::new(f).lines();

    // header：format 不识别 / version 高于本实现 → 明确报错退出
    let first = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("空文件，不是 niri-clip 导出文件"))??;
    let header: ImportHeader = serde_json::from_str(&first)
        .with_context(|| format!("首行不是 niri-clip 导出头：{}", trunc_str(&first, 80)))?;
    if header.format != EXPORT_FORMAT {
        anyhow::bail!("format 不识别：{}（期望 {}）", header.format, EXPORT_FORMAT);
    }
    if header.version > EXPORT_VERSION {
        anyhow::bail!(
            "文件版本 v{} 高于本实现 v{}，请升级 niri-clip",
            header.version,
            EXPORT_VERSION
        );
    }

    let mut out = ImportOutcome::default();
    let warn = |msg: String| {
        eprintln!("[niri-clip import] 跳过无效条目：{msg}");
    };

    if dry_run {
        let conn = open_existing_history()?;
        let snapshot = conn
            .as_ref()
            .map(Connection::unchecked_transaction)
            .transpose()?;
        let mut history = None;
        if let Some(tx) = snapshot.as_ref() {
            let has_clips: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='clips' AND type='table')",
                [],
                |r| r.get(0),
            )?;
            if has_clips {
                let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
                history = Some((tx, version < 4));
            }
        }
        // 同文件重复 hash 与实际执行同口径：实际路径第二条因已入本事务计
        // exists，dry-run 侧用 seen 集合收敛，保证预览统计可对照
        let mut seen = std::collections::HashSet::new();
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match parse_validate(&line) {
                Ok(e) => {
                    let hit = match history {
                        Some((tx, legacy_hash)) => entry_exists(tx, &e, legacy_hash)?,
                        None => false,
                    };
                    if hit || !seen.insert(entry_hash(&e).to_owned()) {
                        out.exists += 1;
                    } else {
                        out.imported += 1;
                    }
                }
                Err(msg) => {
                    out.invalid += 1;
                    warn(msg);
                }
            }
        }
        return Ok(out);
    }

    // 实际写入：单事务原子提交（中途硬错误整体回滚，valid 条目不半途而废）
    let cfg = Config::load();
    let mut conn = crate::store::connect()?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    for line in lines {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match parse_validate(&line) {
            Ok(e) => {
                // 同文件内重复 hash 也在此收敛：上一条已入本事务，查询可见
                if entry_exists(&tx, &e, false)? {
                    out.exists += 1;
                    continue;
                }
                insert_entry(&tx, e)?;
                out.imported += 1;
            }
            Err(msg) => {
                out.invalid += 1;
                warn(msg);
            }
        }
    }
    let paths = trim_history(&tx, cfg.max_items)?;
    tx.commit()?;
    remove_image_files(&paths);
    Ok(out)
}

fn open_existing_history() -> Result<Option<Connection>> {
    for path in [Config::db_path(), Config::legacy_db_path()] {
        if path.try_exists()? {
            let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .with_context(|| format!("只读打开 {}", path.display()))?;
            conn.busy_timeout(std::time::Duration::from_millis(5000))?;
            return Ok(Some(conn));
        }
    }
    Ok(None)
}

fn entry_exists(conn: &Connection, entry: &ValidEntry, legacy_hash: bool) -> Result<bool> {
    // Pre-v4 text hashes will be replaced by blake3(text) during the real import.
    // Use text equality to predict that migration without modifying the old database.
    let (sql, value) = match entry {
        ValidEntry::Text { text, .. } if legacy_hash => (
            "SELECT EXISTS(SELECT 1 FROM clips WHERE text=?1 AND (hash IS NULL OR hash NOT LIKE 'img:%'))",
            text.as_str(),
        ),
        _ => (
            "SELECT EXISTS(SELECT 1 FROM clips WHERE hash=?1)",
            entry_hash(entry),
        ),
    };
    Ok(conn.prepare_cached(sql)?.query_row([value], |r| r.get(0))?)
}

fn entry_hash(e: &ValidEntry) -> &str {
    match e {
        ValidEntry::Text { hash, .. } | ValidEntry::Image { hash, .. } => hash,
    }
}

fn trunc_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
