mod config;
mod daemon;
mod preview;
mod store;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// 预览指定 id
    Preview { id: i64 },
    /// 切换固定
    Pin { id: i64 },
    /// 删除指定条目（--force/-f 跳过星标确认，供脚本与无头环境使用）
    Delete {
        id: i64,
        #[arg(short, long)]
        force: bool,
    },
    /// 清空历史
    Wipe,
    /// 从 cliphist 迁移
    Migrate,
    /// 查看状态
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Daemon) => daemon::run().await?,
        Some(Commands::Tui) => tui::run()?,
        Some(Commands::Store) => daemon::store_from_stdin()?,
        Some(Commands::InstallService) => {
            let path = daemon::install_service()?;
            println!("已写入 {}", path.display());
            println!("下一步执行：");
            println!("  systemctl --user daemon-reload");
            println!("  systemctl --user enable --now niri-clip.service");
            println!("查看日志： journalctl --user -u niri-clip -f");
        }
        Some(Commands::ListRaw) => tui::list_raw()?,
        Some(Commands::Preview { id }) => tui::preview_id(id)?,
        Some(Commands::Pin { id }) => {
            let pinned = store::toggle_pin(id)?;
            let msg = if pinned {
                "已固定"
            } else {
                "已取消固定"
            };
            let _ = notify_rust::Notification::new()
                .summary("niri-clip")
                .body(&format!("{} {}", msg, id))
                .show();
            println!("{} {}", msg, id);
        }
        Some(Commands::Delete { id, force }) => {
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
                    println!("cancelled");
                    return Ok(());
                }
            }
            store::delete(id)?;
            println!("deleted {}", id);
        }
        Some(Commands::Wipe) => {
            store::wipe()?;
            println!("wiped");
        }
        Some(Commands::Migrate) => {
            let n = store::migrate_from_cliphist()?;
            println!("migrated {} items from cliphist", n);
        }
        Some(Commands::Status) => {
            let cfg = config::Config::load();
            println!(
                "niri-clip v{} - {:?}",
                env!("CARGO_PKG_VERSION"),
                config::Config::db_path()
            );
            println!("config: {:?}", cfg);
            let clips = store::list(5)?;
            println!("recent {} clips:", clips.len());
            for c in clips {
                println!(
                    "  {} {} {}",
                    if c.pinned { "★" } else { " " },
                    c.id,
                    preview::preview_text(&c, 60)
                );
            }
        }
        None => {
            tui::run()?;
        }
    }
    Ok(())
}
