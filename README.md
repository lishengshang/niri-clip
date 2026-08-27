# niri-clip — 为 niri + Wayland 设计的高性能剪贴板历史

> 开箱即用 · 单进程 fzf · 删除不跳顶 · Rust 未来可期

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Wayland](https://img.shields.io/badge/Wayland-niri-4a90e2)](https://github.com/YaLTeR/niri)

## 1. 痛点

`niri` 自带的 `cliphist + fzf + bash while true` 方案：每次 `Ctrl+X` 删除或新复制，`fzf` 重启，光标强制回到第一行。批量清理时极其痛苦。

**niri-clip Plan B 解决：** 单 `fzf` 进程 `reload-sync + --track --id-nth`，删除后光标停在 **下一个**，删最后一行停在 **上一个**，搜索词/滚动不丢失。

## 2. 快速开始 (v0.3 - Rust)

```bash
# 安装
cargo install --path ~/Projects/niri-clip --force
# 配置
cat ~/.config/niri-clip/config.toml
# 状态
niri-clip status
# 迁移旧数据
niri-clip migrate
# TUI (Mod+V)
niri-clip tui  # fzf --track 不跳顶，缺 kitty 自动 fuzzel

# 兼容：旧 bash Plan B 仍在 ~/.config/niri/scripts/clipboard-history-tui.sh
# 回滚：
git -C ~/dotfiles diff home/.config/niri/scripts/clipboard-history-tui.sh
```

依赖：`fzf>=0.44` 或 `fuzzel`, `wl-clipboard`, `kitty` (可选), `nirius`

## 3. 项目结构

```
niri-clip/ (v0.3 Rust)
├── src/
│   ├── main.rs       # daemon/tui/store/list-raw/preview/pin/delete/wipe/migrate/status
│   ├── config.rs     # ~/.config/niri-clip/config.toml
│   ├── store.rs      # SQLite WAL + FTS5 + hash去重 + 300懒加载 + 缓存
│   ├── daemon.rs     # wl-clipboard-rs 原生轮询 500ms + 回退 wl-paste
│   ├── tui.rs        # fzf --track --id-nth + fuzzel 回退 + chafa
│   └── preview.rs    # 截断 + chafa/kitty 图片预览
├── scripts/          # legacy Bash Plan B (已部署)
│   └── clipboard-history-tui.sh
├── config/config.toml.example
├── assets/{niri-clip.kdl,niri-clip.service}
└── docs/{PLAN,ARCHITECTURE,ROADMAP,CHANGELOG}.md
```

## 4. Plan B 原理

旧：`while true; do build_menu | fzf; ...; continue; done` → 每次 `fork + reload` → `pos(1)`

新：`build_menu | fzf --track --id-nth 2 --bind 'ctrl-x:execute-silent(delete.sh {2})+reload-sync(build.sh)'`

- `--track --id-nth 2` 以 `id` 为主键跨 `reload` 追踪
- `reload-sync` 同步刷新，无闪白
- `execute-silent` 删除不退出 `fzf`，`reload` 后 `fzf` 自动把光标放在同索引 (即下一个)

详见 `docs/PLAN.md`.

## 5. 路线图

- **v0.1 ✅** – Bash Plan B，修复跳顶 (`Mod+V` 不跳了)
- **v0.2 ✅** – Rust：SQLite + fzf不跳顶 + fuzzel回退 + config + daemon
- **v0.3 ✅** – 原生 `wl-clipboard-rs` + 300懒加载 + chafa + tests/manual.sh
- **v1.0** – AUR、waybar、man、CI

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
