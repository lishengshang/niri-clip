//! 单实例保护与已开窗口聚焦。
//!
//! 实例互斥机制已收敛到 `niri_clip_core::single_instance`（任务 2.6 / C4）：
//! flock 由内核在进程退出或崩溃时自动释放，不再需要 PID 文件与 `/proc/{pid}`
//! 复核，也没有 PID 回收造成的假阳性。本模块只保留"已有实例时做什么"。

use niri_clip_core::single_instance::InstanceGuard;

/// 取得 GUI 单实例锁。返回的守卫**必须存活到进程退出**（drop 即释放锁）。
///
/// 若已有实例在跑：经 niri IPC 聚焦它的窗口后本进程退出（不返回）——
/// Mod+V 连按应"拉回已开窗口"而非开新的。
pub fn ensure_single_instance() -> Option<InstanceGuard> {
    match InstanceGuard::try_acquire("gui") {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => {
            let _ = focus_existing_window();
            // 此处尚未创建 iced 运行时与 sqlite 连接，没有可析构的资源，
            // 故用 process::exit 直接退出；app 启动后的退出点（update.rs）
            // 已改为 iced::exit() 以走正常析构（任务 2.6 / D3）
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("[niri-clip gui] single instance lock failed: {e:#}");
            None
        }
    }
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
        let hit = wins
            .iter()
            .find(|w| w.app_id.as_deref() == Some("niri-clip-gui"));
        assert_eq!(hit.map(|w| w.id), Some(17), "取首个命中的窗口");
    }

    #[test]
    fn json_windows_tolerates_missing_app_id() {
        let sample = br#"[{"id":2},{"id":3,"app_id":null}]"#;
        let wins: Vec<NiriWindow> = serde_json::from_slice(sample).unwrap();
        assert_eq!(wins.len(), 2);
    }
}
