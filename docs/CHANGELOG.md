# Changelog

## v0.2.0 - 2026-08-27

### Added
- Rust 重写：`config` / `store` / `daemon` / `tui` / `preview` 模块
- `~/.config/niri-clip/config.toml` 配置化 (max_items, preview_width, ignore_regex, tui_backend)
- `niri-clip daemon` 守护进程：`wl-paste --watch niri-clip store` → SQLite WAL
- `niri-clip tui` 单进程 fzf `--track --id-nth 2` + `reload-sync`，删除不跳顶，支持 `fuzzel` 回退
- `niri-clip {store,list-raw,preview,pin,delete,wipe,migrate,status}` 子命令

### Changed
- `clipboard-history-ui.sh` 自动切 Rust TUI (有 `niri-clip` 就用 `niri-clip tui`)
- `clipboard-history.sh` 自动切 Rust daemon
- 分支 `master` → `main`

### Fixed
- `Mod+V` 删除后跳回顶部的问题 (Plan B)
- `config.toml` 容错：缺字段时回退默认值

## v0.1.0 - 2026-08-27

- Bash Plan B：单进程 fzf + track + id-nth 修复跳顶
- 脚本部署到 `~/.config/niri/scripts/`
- 项目初始化
