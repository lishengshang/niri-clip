//! niri-clip 核心库：CLI 与原生 UI 共用的全部业务逻辑。
//!
//! 分层约定（见 docs/NATIVE-UI.md）：语义收敛在本 crate（存储/捕获/当前项
//! 指针/预览），UI 层（fzf TUI / iced 原生窗口）只做渲染与输入分发。

pub mod config;
pub mod daemon;
pub mod notify;
pub mod preview;
pub mod store;
pub mod tui;

/// 测试专用：全局 XDG 环境变量锁。store/config/tui 的测试都会临时改写
/// XDG_* 环境变量做目录隔离，必须共享同一把锁串行化——各自持有独立锁
/// 时并行测试会互相踩踏（真机实锤：新增 tui 测试后 store 套件间歇全红）
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::Mutex;
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
}
