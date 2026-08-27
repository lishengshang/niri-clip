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
    /// 从 stdin 读取并入库 (供 wl-paste --watch 调用)
    Store,
    /// 列出历史 (供 fzf reload 调用)
    #[command(name = "list-raw")]
    ListRaw,
    /// 预览指定 id
    Preview { id: i64 },
    /// 切换固定
    Pin { id: i64 },
    /// 删除指定 id
    Delete { id: i64 },
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
        Some(Commands::Delete { id }) => {
            // 星标二次确认在 TUI 层已处理，这里直接删
            // 但为安全，若 pinned 则弹 fuzzel 确认
            if store::is_pinned(id).unwrap_or(false) {
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
