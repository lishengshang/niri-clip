//! 桌面通知 helper：notify-send 子进程（任务 1.8 决策项②，替换 notify-rust/zbus）。
//!
//! 取舍（详见 ARCHITECTURE §9）：实际 API 面仅需 summary/body，换子进程后
//! CLI 主包闭包 -81 crate（zbus/zvariant D-Bus 栈出图）；代价是运行时依赖
//! `notify-send`（libnotify 包），缺失时静默失败——与原 `let _ =` 吞错误
//! 语义一致，且全部调用方均有 `notify_enabled` 门控。
//!
//! 后台线程 spawn + wait 收尸：调用方零阻塞（GUI 侧无 UI 冻结面）、
//! daemon 长驻进程不留僵尸；通知为低频路径（超限/失败反馈等），
//! 每条一线程成本可忽略。参数走数组不经 shell，无注入面。

use std::process::Command;

/// 发送桌面通知，summary 固定为 `niri-clip`。不阻塞、不报错：
/// 无通知服务 / notify-send 缺失时静默。
pub fn send(body: &str) {
    let body = body.to_owned();
    std::thread::spawn(move || {
        let _ = Command::new("notify-send")
            .args(["--app-name=niri-clip", "niri-clip", &body])
            .status();
    });
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
