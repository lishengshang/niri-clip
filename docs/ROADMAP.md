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

## v0.3 - Polish (1周内)

- [ ] `daemon` 切 `wl-clipboard-rs` 原生 (不再 fork wl-paste)
- [ ] `build-menu` 缓存 + `limit 300` 懒加载优化
- [ ] 性能压测：10k 条搜索 <50ms
- [ ] `tests/manual.sh` 自动验证光标跟随

## v1.0 - Production

- [ ] `ignore_regex` 安全过滤强化 (1Password / OTP)
- [ ] 图片剪贴板完整支持
- [ ] AUR `niri-clip-bin` + `niri-clip-git`
- [ ] `waybar` 模块 + `man` 页 + CI

## 长期

- [ ] `OSC52` 远程
- [ ] `niri overview` 集成
- [ ] 加密历史 (age)
