use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "niri-clip", version, about = "高性能 niri 剪贴板历史")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// 启动后台守护进程 (wl-paste watcher)
    Daemon,
    /// 打开 TUI (Mod+V)
    Tui,
    /// 清空历史
    Wipe,
    /// 查看状态
    Status,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Daemon) => daemon::run().await?,
        Some(Commands::Tui) => tui::run()?,
        Some(Commands::Wipe) => store::wipe()?,
        Some(Commands::Status) => {
            println!("niri-clip v{} - daemon + tui", env!("CARGO_PKG_VERSION"));
            println!("db: {:?}", store::db_path());
        }
        None => {
            // 默认打开 TUI，方便 Mod+V 直接 spawn
            tui::run()?;
        }
    }
    Ok(())
}

mod daemon {
    pub async fn run() -> anyhow::Result<()> {
        println!("[niri-clip daemon] TODO: wl-clipboard-rs watcher + sqlite store");
        println!("v0.1 阶段仍由 bash + cliphist 提供能力，Rust daemon 在 v1.0 接管");
        // 占位：v0.1 实际调用 legacy bash watcher
        // v1.0 将实现：监听 wl-clipboard, 写入 SQLite WAL, FTS5 索引, 正则过滤
        tokio::signal::ctrl_c().await?;
        Ok(())
    }
}

mod tui {
    pub fn run() -> anyhow::Result<()> {
        // v0.1 直接 exec legacy bash TUI (单进程 fzf + track + reload)
        let script = dirs::config_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/home/mio/.config"))
            .join("niri/scripts/clipboard-history-tui.sh");
        if script.exists() {
            let status = std::process::Command::new("bash").arg(&script).status()?;
            std::process::exit(status.code().unwrap_or(0));
        }
        anyhow::bail!("legacy TUI not found: {:?}", script);
    }
}

mod store {
    pub fn db_path() -> std::path::PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join("niri-clip/db.sqlite")
    }
    pub fn wipe() -> anyhow::Result<()> {
        let p = db_path();
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
        // 同时兼容 cliphist
        let _ = std::process::Command::new("cliphist").arg("wipe").status();
        println!("wiped {:?}", p);
        Ok(())
    }
}
