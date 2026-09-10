use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use niri_clip_core::{config, daemon, preview, store, tui};

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
    /// 删除指定条目（--force/-f 跳过星标确认；--fzf 走 fzf 内嵌二段确认）
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
            if fzf {
                // fzf 内嵌二段确认（任务 1.5）：★ 行第一次按仅挂起
                // （list-raw reload 后该行打 "再按Ctrl-X确认删除" 标记），
                // 同行再按才真删；无 fuzzel 依赖。挂起时静默（execute-silent）
                match tui::delete_with_fzf_confirm(id)? {
                    tui::DeleteConfirm::Deleted => outln!("deleted {}", id),
                    tui::DeleteConfirm::Pending => {}
                }
                return Ok(());
            }
            // 星标条目默认要求 GUI 确认；--force 供脚本/CI 等无头场景。
            // 无头环境的 CI 已由 smoke job 用 -f 覆盖（issue #2 评审项）
            if !force && store::is_pinned(id).unwrap_or(false) {
                let has_fuzzel = which::which("fuzzel").is_ok();
                if !has_fuzzel {
                    eprintln!(
                        "[niri-clip] 星标条目需要确认但 fuzzel 不可用，已取消；无头环境请使用 delete --force"
                    );
                    println!("cancelled");
                    return Ok(());
                }
                let choice = std::process::Command::new("fuzzel")
                    .args(["--dmenu", "--lines=2", "--width=18", "--prompt=删除星标? "])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .ok()
                    .and_then(|mut c| {
                        use std::io::Write;
                        let _ = c
                            .stdin
                            .as_mut()
                            .unwrap()
                            .write_all("取消\n确认\n".as_bytes());
                        c.wait_with_output().ok()
                    })
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                if choice != "确认" {
                    outln!("cancelled");
                    return Ok(());
                }
            }
            store::delete(id)?;
            outln!("deleted {}", id);
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
        Some(Commands::Migrate) => {
            let n = store::migrate_from_cliphist()?;
            outln!("migrated {} items from cliphist", n);
        }
        Some(Commands::Status) => {
            let cfg = config::Config::load();
            outln!(
                "niri-clip v{} - {:?}",
                env!("CARGO_PKG_VERSION"),
                config::Config::db_path()
            );
            outln!("config: {:?}", cfg);
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
