# Changelog

## v0.3.0 - 2026-08-27

### Added
- daemon 原生 `wl-clipboard-rs` 轮询 (500ms)，不再 `fork wl-paste`，失败自动回退到 `wl-paste --watch`
- store 懒加载 `TUI_LIMIT=300` + 200ms 缓存，`invalidate_cache` 在 insert/delete/pin/wipe 时失效
- `store::bench_10k()` 10k 条压测，实测 `list 300 <11ms` / `sqlite 10k <4ms`
- tui `chafa` 图片预览：`enable_image_preview=true` 时 `preview` 尝试 `chafa --format symbols`
- `tests/manual.sh` 自动造 20 条验证删除后 `pos` 跟随 + 压测 + 图片配置检查

### Changed
- `config.toml.example` 默认 `enable_image_preview=true`
- `tui::list_raw` 处理 `Broken pipe` (head -n5) 不 panic，`writeln` 忽略错误
- `store::list` 新增 `list_tui()` 缓存层，`tui` 自动切 300 条

### Fixed
- `list-raw | head` 导致的 `Broken pipe` panic

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
