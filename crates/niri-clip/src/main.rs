use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use niri_clip_core::{backup, config, confirm, daemon, preview, store};

// TUI 后端（fzf/fuzzel 编排、终端探测、fzf 版本门控）属 CLI 职责，故住在 CLI
// crate 而非 core——core 保持"纯逻辑库、不含 UI 后端选择"的分层
// （见 docs/ARCHITECTURE.md §1 分层边界 / §10；任务 2.6 D1 落地）
mod tui;

#[derive(Parser)]
#[command(name = "niri-clip", version, about = "高性能 niri 剪贴板历史")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动后台守护进程 (wl-paste watcher -> SQLite)
    Daemon,
    /// 打开 TUI (Mod+V) - 自动选 fzf/fuzzel，支持 --track 不跳顶
    Tui,
    /// 从 stdin 读取并入库 (v0.4.1 主捕获链路：wl-paste --watch 管道直灌)
    Store,
    /// 安装 systemd user 单元到 ~/.config/systemd/user/（随后 enable --now 即可托管）
    InstallService,
    /// 列出历史 (供 fzf reload 调用)
    #[command(name = "list-raw")]
    ListRaw,
    /// 全库全文搜索（FTS5 trigram，中英文子串均命中；输出同 list-raw 格式）
    Search {
        query: String,
        #[arg(long, default_value_t = store::SEARCH_LIMIT)]
        limit: usize,
    },
    /// 预览指定 id
    Preview { id: i64 },
    /// 复制指定 id 到剪贴板 (供 TUI 快选)
    Copy { id: i64 },
    /// 切换固定
    Pin { id: i64 },
    /// 删除指定条目。星标条目需二次确认：15 秒内重复执行同一命令即删除
    /// （状态由 core 统一管理，语义见 ADR-005）；--force/-f 跳过确认直删；
    /// --fzf 由 fzf 绑定调用（挂起时静默，挂起态经 list-raw 行尾标记呈现）
    Delete {
        id: i64,
        #[arg(short, long)]
        force: bool,
        #[arg(long)]
        fzf: bool,
    },
    /// 清空历史
    Wipe,
    /// 数据统计（条数/体积/图片占比，库与图片均为磁盘实测口径）
    Stats,
    /// VACUUM 压缩库文件（回收删除/淘汰留下的空洞，返回前后体积）
    Vacuum,
    /// 删除早于指定日期的旧条目（星标与当前项受保护，同图片 GC 语义）
    Prune {
        /// YYYY-MM-DD，本地时区当日零点之前
        #[arg(long)]
        before: String,
        /// 只统计将删除的内容，不实际删除
        #[arg(long)]
        dry_run: bool,
    },
    /// 全量导出历史为 NDJSON（首行元数据 + 每行一个条目，图片内嵌 base64；
    /// hash 幂等合并键，格式见 ADR-004）
    Export {
        /// 输出文件路径；`-` 写 stdout（可管道 `| gzip`）
        path: String,
        /// 额外用 VACUUM INTO 产出完整 db.sqlite 物理快照（已存在则报错）
        #[arg(long)]
        sqlite: Option<std::path::PathBuf>,
    },
    /// 从 NDJSON 备份回灌合并（hash 幂等：已存在跳过不刷时序；损坏条目跳过并警告）
    Import {
        /// 备份文件路径（niri-clip export 产物）
        path: String,
        /// 只校验并统计，不写入
        #[arg(long)]
        dry_run: bool,
    },
    /// 从 cliphist 迁移
    Migrate,
    /// 查看状态
    Status,
    /// 生成 shell 补全脚本到 stdout（打包安装用：bash|zsh|fish|elvish|powershell）
    Completions { shell: Shell },
    /// 输出 man page 到 stdout（打包安装用：> niri-clip.1）
    Man,
}

/// println! 的 EPIPE 安全版：stdout 管道被下游截断（`niri-clip status | head`）
/// 时 Rust 默认 SIGPIPE=SIG_IGN，写入返回 EPIPE 而 println! 会 panic。
/// CLI 输出仅是给人看的，写失败静默忽略即可
macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

/// 人类可读字节口径（stats/vacuum/prune 输出用），1024 进制与文档 MiB 口径一致
fn fmt_bytes(b: u64) -> String {
    const K: f64 = 1024.0;
    let b = b as f64;
    if b >= K * K * K {
        format!("{:.1} GiB", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.1} MiB", b / (K * K))
    } else if b >= K {
        format!("{:.1} KiB", b / K)
    } else {
        format!("{} B", b as u64)
    }
}

/// 毫秒时间戳 -> 本地时区 YYYY-MM-DD（stats 最旧/最新条目展示用）；
/// 非法值（理论上不存在）退回原始数字，不因展示挂掉命令
fn fmt_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| ms.to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    use std::io::Write as _;
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Daemon) => daemon::run().await?,
        Some(Commands::Tui) => tui::run()?,
        Some(Commands::Store) => daemon::store_from_stdin()?,
        Some(Commands::InstallService) => {
            let path = daemon::install_service()?;
            outln!("已写入 {}", path.display());
            outln!("下一步执行：");
            outln!("  systemctl --user daemon-reload");
            outln!("  systemctl --user enable --now niri-clip.service");
            outln!("查看日志： journalctl --user -u niri-clip -f");
        }
        Some(Commands::ListRaw) => tui::list_raw()?,
        Some(Commands::Search { query, limit }) => tui::search_raw(&query, limit)?,
        Some(Commands::Preview { id }) => tui::preview_id(id)?,
        Some(Commands::Copy { id }) => {
            store::copy_to_clipboard(id)?;
            outln!("copied {}", id);
        }
        Some(Commands::Pin { id }) => {
            let pinned = store::toggle_pin(id)?;
            let msg = if pinned {
                "已固定"
            } else {
                "已取消固定"
            };
            if config::Config::load().notify_enabled {
                niri_clip_core::notify::send(&format!("{} {}", msg, id));
            }
            outln!("{} {}", msg, id);
        }
        Some(Commands::Delete { id, force, fzf }) => {
            // 星标条目的二段确认统一走 core 状态机（任务 2.6 / ADR-005）：
            // 15s TTL 落盘，fzf TUI / 原生 UI / CLI 共用同一语义，前端只负责呈现。
            // --force 供脚本与无头环境显式绕过（不再依赖 fuzzel 弹窗——ADR-005）
            if force {
                store::delete(id)?;
                outln!("deleted {}", id);
                return Ok(());
            }
            match confirm::request(id)? {
                confirm::Decision::Deleted => outln!("deleted {}", id),
                confirm::Decision::Pending => {
                    // fzf 内嵌路径由 list-raw 的行尾标记呈现挂起态，无需文案；
                    // 其它调用方（脚本/手工）必须被告知"这只是第一段"，否则会
                    // 误以为删除失败
                    if !fzf {
                        outln!(
                            "pending: ★ 条目需二次确认，15 秒内再次执行同一命令即删除（或加 --force）"
                        );
                    }
                }
            }
        }
        Some(Commands::Wipe) => {
            store::wipe()?;
            outln!("wiped");
        }
        Some(Commands::Stats) => {
            let s = store::stats()?;
            outln!(
                "条目 {}（星标 {}，图片 {}）",
                s.total,
                s.pinned,
                s.image_entries
            );
            let total = s.db_bytes + s.images_disk_bytes;
            let pct = (s.images_disk_bytes * 100).checked_div(total).unwrap_or(0);
            outln!(
                "库 {} + 图片 {} = {}（图片占 {}%）",
                fmt_bytes(s.db_bytes),
                fmt_bytes(s.images_disk_bytes),
                fmt_bytes(total),
                pct
            );
            match (s.oldest_ts, s.newest_ts) {
                (Some(o), Some(n)) => outln!("最旧 {} · 最新 {}", fmt_date(o), fmt_date(n)),
                _ => outln!("库为空"),
            }
        }
        Some(Commands::Vacuum) => {
            let (before, after) = store::vacuum()?;
            outln!(
                "vacuum: {} -> {}（回收 {}）",
                fmt_bytes(before),
                fmt_bytes(after),
                fmt_bytes(before.saturating_sub(after))
            );
        }
        Some(Commands::Prune { before, dry_run }) => {
            let cutoff = store::parse_local_date_ms(&before)?;
            let r = store::prune_before(cutoff, dry_run)?;
            if dry_run {
                outln!(
                    "dry-run: 将删除 {} 条（图片 {}，载荷约 {}）；实际执行请去掉 --dry-run",
                    r.deleted,
                    r.images_deleted,
                    fmt_bytes(r.freed_bytes.max(0) as u64)
                );
            } else {
                outln!(
                    "已删除 {} 条（图片 {}，回收载荷 {}）；库文件体积回落请再执行 niri-clip vacuum",
                    r.deleted,
                    r.images_deleted,
                    fmt_bytes(r.freed_bytes.max(0) as u64)
                );
            }
        }
        Some(Commands::Export { path, sqlite }) => {
            let dest = if path == "-" {
                None
            } else {
                Some(std::path::PathBuf::from(&path))
            };
            let to_stdout = dest.is_none();
            let r = backup::export_json_file(dest.as_deref())?;
            // `-` 时数据流占用 stdout，汇总改走 stderr，保证 `export - | ...`
            // 管道产物是纯 NDJSON（jq/wc/import 才能直接消费）
            let report = |line: String| {
                if to_stdout {
                    eprintln!("{line}");
                } else {
                    outln!("{line}");
                }
            };
            if let Some(snap) = &sqlite {
                backup::export_sqlite(snap)?;
                report(format!("快照: {}", snap.display()));
            }
            report(format!(
                "已导出 {} 条（图片 {}，图片载荷 {}）→ {}",
                r.count,
                r.images,
                fmt_bytes(r.image_bytes),
                if to_stdout { "stdout" } else { &path }
            ));
        }
        Some(Commands::Import { path, dry_run }) => {
            let r = backup::import_file(std::path::Path::new(&path), dry_run)?;
            if dry_run {
                outln!(
                    "dry-run: 将导入 {} 条（已存在 {}，无效 {}）；实际执行请去掉 --dry-run",
                    r.imported,
                    r.exists,
                    r.invalid
                );
            } else {
                outln!(
                    "已导入 {} 条（已存在 {}，无效 {}）",
                    r.imported,
                    r.exists,
                    r.invalid
                );
            }
        }
        Some(Commands::Migrate) => {
            let n = store::migrate_from_cliphist()?;
            outln!("migrated {} items from cliphist", n);
        }
        Some(Commands::Status) => {
            let cfg = config::Config::load();
            outln!(
                "niri-clip v{} - {}",
                env!("CARGO_PKG_VERSION"),
                config::Config::db_path().display()
            );
            // 只列用户真正关心的项。此前打印整个 Config 的 Debug 表示
            // （含 `ignore_re: Some(Regex(...))` 的内部结构），既难读也无用
            // （任务 2.6 / D9）
            outln!("config: {}", config::Config::path().display());
            outln!("  条目上限 max_items={}", cfg.max_items);
            outln!("  后端 tui_backend={}", cfg.tui_backend);
            outln!(
                "  预览 enable_preview={} · 图片捕获 enable_image_preview={}",
                cfg.enable_preview,
                cfg.enable_image_preview
            );
            outln!("  通知 notify_enabled={}", cfg.notify_enabled);
            outln!(
                "  体积上限 max_clip_bytes={} max_image_bytes={} max_image_total_bytes={}",
                cfg.max_clip_bytes,
                cfg.max_image_bytes,
                cfg.max_image_total_bytes
            );
            let clips = store::list(5)?;
            outln!("recent {} clips:", clips.len());
            for c in clips {
                outln!(
                    "  {} {} {}",
                    if c.pinned { "★" } else { " " },
                    c.id,
                    preview::preview_text(&c, 60)
                );
            }
        }
        Some(Commands::Completions { shell }) => {
            // 生成器内部对 EPIPE 直接 panic（clap_complete shells/shell.rs），
            // 先写内存缓冲再忽略错误输出，对齐 outln! 的"写失败不 panic"口径
            let mut cmd = Cli::command();
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut cmd, "niri-clip", &mut buf);
            let _ = std::io::stdout().write_all(&buf);
        }
        Some(Commands::Man) => {
            let mut buf = Vec::new();
            clap_mangen::Man::new(Cli::command()).render(&mut buf)?;
            let _ = std::io::stdout().write_all(&buf);
        }
        None => {
            tui::run()?;
        }
    }
    Ok(())
}
