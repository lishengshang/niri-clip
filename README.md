# niri-clip — 为 niri + Wayland 设计的高性能剪贴板历史

> 开箱即用 · 单进程 fzf · 删除不跳顶 · Rust 未来可期

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Wayland](https://img.shields.io/badge/Wayland-niri-4a90e2)](https://github.com/YaLTeR/niri)

## 1. 痛点

`niri` 自带的 `cliphist + fzf + bash while true` 方案：每次 `Ctrl+X` 删除或新复制，`fzf` 重启，光标强制回到第一行。批量清理时极其痛苦。

**niri-clip Plan B 解决：** 单 `fzf` 进程 `reload-sync + --track --id-nth`，删除后光标停在 **下一个**，删最后一行停在 **上一个**，搜索词/滚动不丢失。

## 2. 快速开始 (v0.1 - Bash 实现，零依赖迁移)

```bash
# 已为 mio 部署到 ~/.config/niri/scripts/
# 直接 Mod+V 体验新版
# 手动测试：
bash ~/.config/niri/scripts/clipboard-history-tui.sh

# 回滚旧版：
git -C ~/dotfiles diff home/.config/niri/scripts/clipboard-history-tui.sh
```

依赖：`cliphist`, `fzf>=0.44`, `fuzzel`, `wl-clipboard`, `kitty`, `nirius`

## 3. 项目结构

```
niri-clip/
├── scripts/                  # v0.1 Bash 实现 (已部署到 niri)
│   └── clipboard-history-tui.sh  # Plan B 单进程 fzf
├── src/                      # v1.0 Rust 高性能实现 (规划中)
│   ├── main.rs               # CLI: daemon / tui / wipe / status
│   ├── daemon/               # wl-clipboard-rs watcher
│   ├── store/                # SQLite WAL + FTS5
│   └── tui/                  # ratatui / fzf --listen
├── config/
│   └── config.toml           # max_items, ignore_regex
├── assets/
│   ├── niri-clip.kdl         # binds.kdl include
│   └── niri-clip.service     # systemd user service
└── docs/
    ├── PLAN.md
    ├── ARCHITECTURE.md
    └── ROADMAP.md
```

## 4. Plan B 原理

旧：`while true; do build_menu | fzf; ...; continue; done` → 每次 `fork + reload` → `pos(1)`

新：`build_menu | fzf --track --id-nth 2 --bind 'ctrl-x:execute-silent(delete.sh {2})+reload-sync(build.sh)'`

- `--track --id-nth 2` 以 `id` 为主键跨 `reload` 追踪
- `reload-sync` 同步刷新，无闪白
- `execute-silent` 删除不退出 `fzf`，`reload` 后 `fzf` 自动把光标放在同索引 (即下一个)

详见 `docs/PLAN.md`.

## 5. 路线图

- **v0.1 (已完成)** – Bash Plan B，修复跳顶，支持 `★` 置顶、预览
- **v0.2** – 配置化、图片预览、性能优化 (limit + 懒加载)
- **v1.0** – Rust 重写：Daemon 常驻 <40MB，SQLite FTS5 10k条 <50ms，支持安全过滤
- **v1.5** – AUR, waybar 模块, SSH OSC52

## 6. 开发

```bash
cd ~/Projects/niri-clip
cargo run -- tui      # 调用 legacy bash TUI
cargo run -- status
cargo build --release # -> target/release/niri-clip
```

## 7. 与 niri 集成

```kdl
// ~/.config/niri/config.kdl
include "niri-clip/niri-clip.kdl"
// niri-clip.kdl 内：
binds {
    Mod+V { spawn "niri-clip" "tui"; }
}
```

## 8. License

MIT
