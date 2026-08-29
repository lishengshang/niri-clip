//! 单实例保护与已开窗口聚焦。

use std::io::Write;

use niri_clip_core::config;

/// 单实例保护：Mod+V 连按会并发拉起多个 GUI。state/gui.lock 存 PID——
/// 活实例存在 → 聚焦其窗口后本进程退出；残留死锁（崩溃/强杀）→ 覆写接管
pub fn ensure_single_instance() {
    let dir = config::Config::state_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("gui.lock");
    let pid = std::process::id();

    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            let _ = write!(f, "{pid}");
        }
        Err(_) => {
            let existing = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|p| *p != pid && pid_alive(*p));
            if existing.is_some() {
                let _ = focus_existing_window();
                std::process::exit(0);
            }
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path)
            {
                let _ = write!(f, "{pid}");
            }
        }
    }
}

/// PID 活性复核：/proc cmdline 含进程名（防 PID 回收误判）
fn pid_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .map(|s| s.replace('\0', " ").contains("niri-clip-gui"))
        .unwrap_or(false)
}

/// 已开窗口聚焦到前台（niri IPC）：Mod+V 二连按 = 把它拉回来而非开新的。
/// `niri msg windows` 输出形如 "Window ID N:" 块 + "  App ID: \"...\"" 行，
/// 逐块扫描取命中 app-id 的窗口 id。
fn focus_existing_window() -> bool {
    let Ok(out) = std::process::Command::new("niri")
        .args(["msg", "windows"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut current_id: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("Window ID ") {
            current_id = rest.split(':').next().map(|s| s.trim().to_string());
        } else if line.contains("App ID:") && line.contains("niri-clip-gui") {
            if let Some(id) = current_id.take() {
                return std::process::Command::new("niri")
                    .args(["msg", "action", "focus-window", "--id", &id])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
            }
        }
    }
    false
}
