# Changelog

## v0.4.0 - 2026-08-27

> 分支 `fix/issue-1-p0-correctness-store-daemon`：全面评估报告 P0 问题闭环。

### Fixed
- **图片预览错位**（P0）：预览不再"取 images 目录最新一张"。schema 引入
  `PRAGMA user_version` 迁移机制（v2），新增 `clips.image_path` 列，
  数据文件按 clip id 写 `images/{id}.bin` 并精确关联渲染
- **并发静默丢数据**（P0）：SQLite 加 `busy_timeout=5000`；
  SELECT 去重检查 + INSERT 收进 `BEGIN IMMEDIATE` 事务原子化，
  消除 fzf 选择旧条目时 wl-copy 与 daemon 并发插入同 hash 的 UNIQUE 冲突竞态；
  daemon/store 的入库错误改为显式记录，不再被 `let _ =` 吞掉
- **daemon 启动探测 panic**（P0）：探测改为单次 `get_contents` 并 match
  ClipboardEmpty/NoMimeType/NoSeats 三类良性错误；旧实现第二次调用
  `unwrap_err()` 在剪贴板恰好可用时直接 panic，systemd 下表现为周期崩启
- **图片等长误判重**：内容指纹改为 FNV-1a64 + mime + 字节长度
  （旧版仅 mime+len，两张等大 PNG 只会收录第一张）
- **TUI 后端门控**：auto 后端不再强制要求 kitty 存在才启用 fzf，
  foot/alacritty 用户恢复 track/reload/pin/delete 完整能力
- 配置路径 fallback 硬编码 `/home/mio/.config` 移除；
  header 提示 "Enter粘贴" 更正为事实行为 "Enter复制"

### Changed
- **数据库迁至 `~/.local/state/niri-clip/db.sqlite`**（XDG state 规范）：
  `~/.cache` 会系统清理工具误删整份历史；首次连接用 `VACUUM INTO`
  一致性快照自动搬迁旧库，旧库保留备份；状态目录 0o700、库文件 0o600
- **复杂度偿还**：移除进程内 200ms 缓存层（fzf reload-sync 每次 spawn 新进程，
  该缓存从未在 reload 路径生效）与未参与任何查询的 FTS 占位表；
  菜单取数统一 `list(min(max_items, TUI_LIMIT))`
- `preview_text` 先廉价截断（O(width)）再换行替换，大文本 reload 不再全文扫描 ×300 行
- 图片数据目录随库迁至 `~/.local/state/niri-clip/images/`；旧孤立时间戳图片不迁移

### Added
- daemon 单实例 flock 锁（state 目录 `daemon.lock`），双开立即报错退出
- 单元测试基础设施：XDG 环境变量隔离临时目录，覆盖去重原子性 /
  busy_timeout 生效 / schema 版本迁移 / 图片关联与内容判重 / 旧库快照搬迁 /
  pin 排序与 limit（共 6 例）
- `Cargo.lock` 入库（PKGBUILD `cargo build --locked` 前置条件）

### Removed
- `store::bench_10k` 死代码；FTS 同步相关的手工维护语句随占位表一并删除

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
