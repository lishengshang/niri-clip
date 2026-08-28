//! niri-clip 原生 layer-shell 前端。
//!
//! M5.2 起按 docs/NATIVE-UI.md 实现（iced_layershell）：
//! 窗口 + 列表渲染 + Enter 复制 + Esc 关闭，随后 M5.3 语义对齐。
//! 当前为 workspace 占位，`niri-clip tui`（fzf/fuzzel）仍为唯一可用后端。
fn main() {
    eprintln!("niri-clip-gui: native backend not implemented yet (M5.2), use `niri-clip tui`");
    std::process::exit(2);
}
