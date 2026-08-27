use anyhow::Result;
use std::io::Read;
use tokio::process::Command;

use crate::config::Config;
use crate::store;

/// `niri-clip store` : 从 stdin 读取剪贴板内容并入库
pub fn store_from_stdin() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        return Ok(());
    }
    // 去掉末尾换行由 cliphist 行为一致
    let inserted = store::insert(buf, None)?;
    if inserted {
        eprintln!("[niri-clip store] inserted");
    } else {
        eprintln!("[niri-clip store] deduplicated/ignored");
    }
    Ok(())
}

/// Daemon: 监听 Wayland 剪贴板
/// v0.2 策略：优先用 `wl-paste --watch niri-clip store`，与 cliphist 兼容且零 Wayland 协议手写
/// 未来 v1.0 再切 wl-clipboard-rs 原生
pub async fn run() -> Result<()> {
    Config::ensure_dirs()?;
    let cfg = Config::load();
    println!("[niri-clip daemon] max_items={} tui={}", cfg.max_items, cfg.tui_backend);
    println!("[niri-clip daemon] db: {:?}", Config::db_path());

    // 检查依赖
    for bin in ["wl-paste", "cliphist"] {
        if which::which(bin).is_err() {
            eprintln!("[warn] missing {}", bin);
        }
    }

    // 如果用户已启用旧的 clipboard-history.enabled，提示迁移
    let enable = dirs::config_dir()
        .unwrap()
        .join("niri/clipboard-history.enabled");
    if enable.exists() {
        eprintln!("[niri-clip] 检测到旧的 clipboard-history.enabled，建议迁移: niri-clip migrate");
    }

    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    // wl-paste --watch 会在每次复制时执行后面的命令，并把剪贴板内容通过 stdin 传给它
    // 正确传参：wl-paste --watch <exe> store  (exe 和 store 分开两个 arg)
    let mut child = Command::new("wl-paste")
        .arg("--watch")
        .arg(&exe)
        .arg("store")
        .spawn()?;

    println!("[niri-clip daemon] watching via wl-paste --watch ...");
    // 同时用 notify 提示
    let _ = notify_rust::Notification::new()
        .summary("niri-clip")
        .body("守护进程已启动")
        .show();

    let status = child.wait().await?;
    eprintln!("[niri-clip daemon] wl-paste exited: {:?}", status);
    Ok(())
}
