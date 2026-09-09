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
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use crate::config::Config;
use crate::store::{
    enforce_max_items, hash_text, image_content_key, tighten_dir_perms, tighten_file_perms,
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

/// 全量导出为 NDJSON。`out = None` 写 stdout（可管道 `| gzip`），
/// `Some(path)` 写文件（0600——内容是全量剪贴板明文）。全表导出不受
/// max_items / TUI_LIMIT 限制。
pub fn export_json_file(out: Option<&Path>) -> Result<ExportOutcome> {
    let mut conn = crate::store::connect()?;
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    // deferred 读事务：COUNT 与全表扫描取同一快照，daemon 并发写不会让
    // header.count 与实际行数互相失真（--sqlite 快照不在本事务内）
    let tx = conn.transaction()?;
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get(0))?;

    let mut w: Box<dyn Write> = match out {
        None => Box::new(BufWriter::new(std::io::stdout().lock())),
        Some(p) => {
            if let Some(parent) = p.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                    tighten_dir_perms(parent);
                }
            }
            let f = std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?;
            tighten_file_perms(p);
            Box::new(BufWriter::new(f))
        }
    };

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
    if path.exists() {
        anyhow::bail!("快照目标已存在，不做覆盖：{}", path.display());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
            tighten_dir_perms(parent);
        }
    }
    let conn = crate::store::connect()?;
    conn.execute("VACUUM INTO ?1", [path.to_string_lossy().as_ref()])
        .with_context(|| format!("VACUUM INTO {}", path.display()))?;
    tighten_file_perms(path);
    Ok(())
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
            let id = tx.last_insert_rowid();
            // 数据文件写入与 insert_image_with 同一模式：.tmp- 先落再原子
            // rename，失败即整事务回滚，不留"有行无图"残缺；异常残留由
            // prune_orphan_images 兜底（.tmp- 前缀在其清扫范围）
            let dir = Config::images_dir();
            std::fs::create_dir_all(&dir)?;
            tighten_dir_perms(&dir);
            let path = dir.join(format!("{id}.bin"));
            let tmp = dir.join(format!(".tmp-import-{id}.bin"));
            std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
            std::fs::rename(&tmp, &path).with_context(|| format!("publish {}", path.display()))?;
            tighten_file_perms(&path);
            tx.execute(
                "UPDATE clips SET image_path=?1 WHERE id=?2",
                params![path.to_string_lossy(), id],
            )?;
        }
    }
    Ok(())
}

/// 从 NDJSON 备份回灌合并。hash 幂等（已存在跳过，不刷新 ts）；每条重算
/// hash 完整性校验，损坏跳过并警告。实际写入在单个 BEGIN IMMEDIATE 事务内
/// 原子提交；不触碰 ▶ 指针；结束按当前配置执行一次 enforce_max_items
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
    let cfg = Config::load();
    let warn = |msg: String| {
        out_done();
        eprintln!("[niri-clip import] 跳过无效条目：{msg}");
    };

    if dry_run {
        let conn = crate::store::connect()?;
        let mut stmt = conn.prepare("SELECT 1 FROM clips WHERE hash=?1")?;
        for line in lines {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match parse_validate(&line) {
                Ok(e) => {
                    let hit: Option<i64> =
                        stmt.query_row(params![entry_hash(&e)], |r| r.get(0)).ok();
                    if hit.is_some() {
                        out.exists += 1;
                    } else {
                        out.imported += 1;
                    }
                }
                Err(msg) => warn(msg),
            }
        }
        return Ok(out);
    }

    // 实际写入：单事务原子提交（中途硬错误整体回滚，valid 条目不半途而废）
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
                let hit: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM clips WHERE hash=?1",
                        params![entry_hash(&e)],
                        |r| r.get(0),
                    )
                    .ok();
                if hit.is_some() {
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
    // Deref 到 &Connection：裁剪是导入的一部分，随整事务原子生效
    enforce_max_items(&tx, cfg.max_items)?;
    tx.commit()?;
    Ok(out)
}

fn entry_hash(e: &ValidEntry) -> &str {
    match e {
        ValidEntry::Text { hash, .. } | ValidEntry::Image { hash, .. } => hash,
    }
}

/// 防止未使用导入的占位（tighten 系列经由本模块重导出使用）
fn out_done() {}

fn trunc_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}
