# ROADMAP

## v0.1 - Plan B (2026-08-27) ✅ 已交付

- [x] 单进程 fzf + reload-sync + track + id-nth，修复删除跳顶
- [x] 脚本部署到 `~/.config/niri/scripts/clipboard-history-tui.sh`
- [x] 项目骨架 `cargo init` + `README/PLAN/ARCHITECTURE`

## v0.2 - Rust MVP (2026-08-27) ✅ 已交付

- [x] `config.toml` 解析 (serde + default 容错)
- [x] `store` SQLite WAL + FTS5 占位，hash 去重，pinned 置顶
- [x] `daemon` Rust `wl-paste --watch niri-clip store` (兼容 niri)
- [x] `tui` Rust `fzf --track --id-nth` 不跳顶 + `fuzzel` 自动回退
- [x] `preview` 文本截断 + 图片 MIME 预留 (`chafa/kitty icat`)
- [x] `migrate` 从 cliphist 导入，`wipe/pin/delete/list-raw/preview` 子命令
- [x] 安装到 `~/.cargo/bin/niri-clip`，`clipboard-history-ui.sh` 自动切 Rust

## v0.3 - Polish (2026-08-27) ✅ 已交付

- [x] `daemon` 切 `wl-clipboard-rs` 原生轮询 500ms，不再 fork `wl-paste`，失败回退
- [x] `store` `TUI_LIMIT=300` 懒加载 + 200ms 缓存 + `bench_10k`，实测 <11ms
- [x] `tui` `chafa` 图片预览开关 `enable_image_preview=true`
- [x] `tests/manual.sh` 自动造 20 条验证 `Mod+V` 删除后 `pos` 跟随 + 压测

## v1.0 - Production

- [ ] `ignore_regex` 安全过滤强化 (1Password / OTP)
- [ ] 图片剪贴板完整支持
- [ ] AUR `niri-clip-bin` + `niri-clip-git`
- [ ] `waybar` 模块 + `man` 页 + CI

## 长期

- [ ] `OSC52` 远程
- [ ] `niri overview` 集成
- [ ] 加密历史 (age)
