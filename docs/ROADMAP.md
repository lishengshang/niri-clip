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

## v0.4 - Correctness & Hygiene ✅ 已交付

> 分支 `fix/issue-1-p0-correctness-store-daemon`。针对全面评估报告的 P0 问题闭环。

### 正确性修复（P0）

- [x] **R1 图片预览错位**：旧实现取 images 目录 mtime 最新文件渲染，必现跨条目串图。
      现 schema 引入 `user_version` 迁移机制，新增 `clips.image_path` 列，
      数据文件按 clip id 落盘 `images/{id}.bin`，预览按条目精确读取
- [x] **R2 并发静默丢数据**：SQLite 增加 `PRAGMA busy_timeout=5000`；
      SELECT 去重 + INSERT 包进 `BEGIN IMMEDIATE` 事务原子化
      （此前 fzf 选中旧条目触发 wl-copy 与 daemon 并发插入同 hash 时，
      一方撞 UNIQUE 报错被上层 `let _ =` 吞掉）；daemon/store 调用点不再吞错
- [x] **R2b daemon 单实例**：新增 state 目录 `daemon.lock`（flock）互斥，
      进程崩溃自动释放，双开立即报错退出
- [x] **R3 daemon 启动探测 panic**：旧实现两次 get_contents 且第二次
      `unwrap_err()`——剪贴板恰在两次之间变为可用即 panic，systemd 下周期崩启；
      改为单次探测 match 三类良性错误（ClipboardEmpty/NoMimeType/NoSeats）
- [x] **R2c 图片等长误判重**：内容指纹改 FNV-1a64 + mime + 字节长度
      （旧版仅 len+mime，两张等大 PNG 只收第一张）

### 数据安全

- [x] **R4 库位置迁 `~/.local/state/niri-clip/`**（XDG state 规范）：
      `~/.cache` 是系统清理工具的目标目录，剪贴板历史属应持久状态。
      连接时检测旧库用 `VACUUM INTO` 一致性快照自动搬迁，旧库保留备份；
      旧时间戳命名的孤立图片不作迁移（本就是错位根源），预览回落占位文本
- [x] 权限收紧：状态目录 0o700、数据库 0o600

### 简化偿还

- [x] 移除进程内 200ms 缓存层（fzf reload-sync 每次 spawn 新进程，缓存从未生效；
      list 300 直查 <11ms）与未参与查询的 FTS 占位表，入口统一 `menu_clips()`
- [x] `preview_text` 先廉价截断再换行替换（旧版大条目在每次 reload 全文扫描 ×300 行）
- [x] TUI 后端门控修正：auto 不再要求 fzf+kitty 同时存在（foot/alacritty 用户
      此前被误降到功能残缺的 fuzzel）；header "Enter粘贴" 更正为 "Enter复制"
- [x] 清除死代码（bench_10k 等）、配置 fallback 路径硬编码（/home/mio 泄漏）
- [x] 新增 6 个单元测试（去重原子性/busy_timeout/图片关联与判重/旧库搬迁/
      pin 排序/min_length 过滤），XDG 环境变量隔离临时目录

## v0.4.1 - Reliable Capture ✅ 已交付

> 分支 `fix/issue-2-daemon-reliable-capture`（堆叠于 issue-1 分支）。
> 线上事故复盘（issue #2）：用户复制内容后历史不再更新——纯轮询实现中
> `read_to_end` 对个别来源应用无限阻塞且不报错，进程存活、捕获停死
> （实测最后一条停在事发前 19 分钟，直至人工介入才被发现）。

- [x] **捕获主路径改事件驱动**：`wl-paste --watch -> sh -c 'exec timeout Ns niri-clip store'`
      —— selection 变化才触发；零空闲往返；每次捕获子进程被 `timeout`
      （新配置 `capture_timeout_secs`，默认 5s）划界，病态挂起秒级回收，
      该故障形态从机制上不可能复现。native 500ms 轮询降级为无 wl-paste 环境兜底
- [x] **图片捕获迁入 store 子命令**：stdin 空时先文本后图片 MIME 探测，
      与 timeout 边界共同生效；非 UTF-8 载荷显式忽略不污染文本库
- [x] **一键 systemd 托管**：新增 `install-service` 子命令写入内置单元模板，
      配合 flock 双实例兜底与 `journalctl --user -u niri-clip -f` 日志链路
- [x] **CI 门禁落地**（GitHub Actions）：fmt --check / clippy -D warnings /
      cargo test --locked / release build --locked / XDG 隔离 CLI 冒烟五道工序

## v0.5 - Quick Select (进行中) — TUI 快速选中

- [x] **A+B 快选**：`1-9` 裸数字 `pos(n)+accept`（`--no-input` 下），`Alt+1..9` 备用，`Space` → `jump` 二段
- [x] **搜索**：`/` 与 `Ctrl-F` → `show-input+clear-query`，`Esc` → `hide-input`，有输入时数字回落 `put(n)`
- [x] **`Ctrl-Y` 复制不退出**：`execute-silent(niri-clip copy {2})` 连挑多条
- [ ] PRIMARY selection 支持（`ClipboardType::Primary`）
- [ ] `max_clip_bytes` 超大条目上限
- [ ] 图片磁盘配额 GC；`notify_enabled` 开关
- [ ] 星标删除 fzf 内嵌确认，去 fuzzel 依赖
- [ ] criterion 基准进 CI；man page / shell 补全

## v1.0 - Production

- [ ] `ignore_regex` 安全过滤强化 (1Password / OTP)
- [ ] 图片剪贴板完整支持
- [ ] AUR `niri-clip-bin` + `niri-clip-git`
- [ ] `waybar` 模块 + `man` 页 + CI

## 长期

- [ ] `OSC52` 远程
- [ ] `niri overview` 集成
- [ ] 加密历史 (age)
