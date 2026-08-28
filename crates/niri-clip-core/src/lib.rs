//! niri-clip 核心库：CLI 与原生 UI 共用的全部业务逻辑。
//!
//! 分层约定（见 docs/NATIVE-UI.md）：语义收敛在本 crate（存储/捕获/当前项
//! 指针/预览），UI 层（fzf TUI / iced 原生窗口）只做渲染与输入分发。

pub mod config;
pub mod daemon;
pub mod preview;
pub mod store;
pub mod tui;
