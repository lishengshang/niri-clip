//! 桌面通知使用 libnotify 的 `notify-send`，仅在调用方启用通知时执行。

use std::process::Command;

/// 发送状态、数量、大小或条目 ID。正文可能出现在锁屏上，调用方须省略剪贴板内容。
///
/// 返回前启动通知子进程，使短命 CLI 退出后仍可投递。后台线程负责回收，
/// coreutils `timeout` 将等待限制在五秒内。通知程序继承 stderr 以便诊断。
pub fn send(body: &str) {
    if let Ok(mut child) = Command::new("timeout")
        .args([
            "5",
            "notify-send",
            "--app-name=niri-clip",
            "niri-clip",
            body,
        ])
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// send() 不得 panic：notify-send 缺失（CI 无桌面会话）时静默。
    /// 只验证调用面安全，不验证投递（投递属系统通知服务职责）。
    #[test]
    fn send_is_safe_without_notification_daemon() {
        send("niri-clip notify helper 测试");
        // 后台线程自行收尾，此处不等待
    }
}
