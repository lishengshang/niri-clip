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

/// PID 活性复核：argv[0] 的文件名必须精确等于 niri-clip-gui（防 PID 回收）。
/// 不用 contains——`cargo build -p niri-clip-gui` 等命令行里含该子串的进程
/// 会造成假阳性拒启
fn pid_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
        .ok()
        .and_then(|s| s.split('\0').next().map(str::to_string))
        .filter(|argv0| !argv0.is_empty())
        .and_then(|argv0| {
            std::path::Path::new(&argv0)
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
        })
        .is_some_and(|name| name == "niri-clip-gui")
}

/// 已开窗口聚焦到前台（niri IPC）：Mod+V 二连按 = 把它拉回来而非开新的。
/// `niri msg -j windows` 输出 JSON 数组，按 app_id 精确匹配（此前解析
/// 人类可读文本，输出格式一变即失效）
#[derive(serde::Deserialize)]
struct NiriWindow {
    id: u64,
    app_id: Option<String>,
}

fn focus_existing_window() -> bool {
    let Ok(out) = std::process::Command::new("niri")
        .args(["msg", "-j", "windows"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let windows: Vec<NiriWindow> = match serde_json::from_slice(&out.stdout) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("[niri-clip gui] parse niri windows failed: {e}");
            return false;
        }
    };
    let Some(w) = windows
        .iter()
        .find(|w| w.app_id.as_deref() == Some("niri-clip-gui"))
    else {
        return false;
    };
    std::process::Command::new("niri")
        .args(["msg", "action", "focus-window", "--id", &w.id.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_windows_parse_picks_app_id() {
        let sample = br#"[{"id":2,"app_id":"kitty"},{"id":17,"app_id":"niri-clip-gui"},{"id":3,"app_id":"niri-clip-gui"}]"#;
        let wins: Vec<NiriWindow> = serde_json::from_slice(sample).unwrap();
        let hit = wins.iter().find(|w| w.app_id.as_deref() == Some("niri-clip-gui"));
        assert_eq!(hit.map(|w| w.id), Some(17), "取首个命中的窗口");
    }

    #[test]
    fn json_windows_tolerates_missing_app_id() {
        let sample = br#"[{"id":2},{"id":3,"app_id":null}]"#;
        let wins: Vec<NiriWindow> = serde_json::from_slice(sample).unwrap();
        assert_eq!(wins.len(), 2);
    }
}
