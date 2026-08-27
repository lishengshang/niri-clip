# ROADMAP

## v0.1 - Plan B (2026-08-27) ✅ 已交付

- [x] 单进程 fzf + reload-sync + track + id-nth，修复删除跳顶
- [x] 脚本部署到 `~/.config/niri/scripts/clipboard-history-tui.sh`
- [x] 项目骨架 `cargo init` + `README/PLAN/ARCHITECTURE`

## v0.2 - Polish (预计 1周)

- [ ] `config.toml` 解析
- [ ] 图片 MIME 探测与预览 (chafa)
- [ ] `fuzzel` 纯 Wayland 模式 (无 kitty)
- [ ] 性能：`build-menu` 缓存 + `cliphist list` limit
- [ ] 测试：`tests/manual.sh` 自动造 20条数据验证光标

## v1.0 - Rust Rewrite (2-3周)

- [ ] `daemon` 用 `wl-clipboard-rs`
- [ ] `store` SQLite WAL + FTS5
- [ ] `tui` 二选一：`fzf --listen` 或 `ratatui`
- [ ] 安全过滤
- [ ] `niri-clip migrate`

## v1.5 - Distribution

- [ ] AUR PKGBUILD
- [ ] `waybar` 模块
- [ ] `man` 页
- [ ] CI: `fmt + clippy + test`

## 长期

- [ ] `OSC52` 远程
- [ ] `niri overview` 集成
- [ ] 加密历史 (age)
