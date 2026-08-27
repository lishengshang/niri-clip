# ARCHITECTURE - niri-clip 全新独立设计

> 目标：完全独立于 cliphist 的高性能 Wayland 剪贴板历史

## 1. 总览

```
┌─────────────────────────────────────────────────────────┐
│ niri (Wayland, ext-data-control / wlr-data-control)     │
└────────────────┬────────────────────────────────────────┘
                 │ Wayland 协议
        ┌────────▼──────────┐ daemon.lock 单实例 flock
        │ daemon (Rust)     │ wl-clipboard-rs 500ms 轮询
        │ tokio + notify    │──────┐
        └────────┬──────────┘      │ hash(ignore_regex) dedup
                 │ insert / insert_image
                 ▼
        ┌───────────────────────────────┐
        │ store (SQLite WAL)            │ BEGIN IMMEDIATE + busy_timeout=5000
        │ clips(hash UNIQUE, image_path)│ PRAGMA user_version 版本化迁移
        │ idx_hash, idx_pinned_ts       │
        └────────┬──────────────────────┘
                 │ list(min(max_items, TUI_LIMIT)) 直查（无缓存层）
        ┌────────▼─────────┐  --track --id-nth 2
        │ tui (fzf)        │  execute-silent + reload-sync
        │ fuzzel 回退      │  chafa 预览 ← images/{id}.bin 按 clip id 关联
        └────────┬─────────┘
                 │ copy (wl-copy)
        ┌────────▼──────────┐
        │ Wayland clipboard │
        └───────────────────┘
```

**独立性：** 单一真相源，不读写 `~/.cache/cliphist/db`，`migrate` 仅一次性导入。

**数据位置：** `~/.local/state/niri-clip/`——`db.sqlite`、`images/`（图片数据文件，
内容不可再生所以随库同置于 state）、`daemon.lock`。
v0.3 及之前位于 `~/.cache/niri-clip/`；连接时检测旧库并用 `VACUUM INTO`
做一致性快照自动搬迁，旧库保留为备份。目录权限 0700 / 库文件 0600。

## 2. Daemon - 原生 Wayland

- **单实例**：启动即对 `state_dir/daemon.lock` 加 `flock(LOCK_EX|LOCK_NB)`，
  双开报错退出；进程崩溃内核自动释放锁，无陈锁残留
- **探测**：单次 `paste::get_contents(Text)` 判定原生通道——`Ok` 或三类良性错误
  （`ClipboardEmpty`/`NoMimeType`/`NoSeats`）均视为可用，其余错误回退。
  （勿复制旧版"调用两次 + 第二次 unwrap_err()"的写法：剪贴板恰在两次调用之间
  变为可用会 panic，systemd Restart 下表现为周期崩启）
- **轮询**：每 500ms `get_contents` 文本；`ClipboardEmpty/NoMimeType/NoSeats` 忽略。
  已知取舍：<500ms 连续复制的丢帧窗口与空闲往返开销；
  v0.5 将改 data-control `SelectionChanged` 事件驱动，轮询降级为兜底配置
- **去重短路**：文本用 `store::hash_text`（与入库同源），图片用
  `store::image_content_key`（FNV1a64+mime+len，跨进程稳定）；
  比对 `last_hash` 相同则跳过入库
- **文本入库**：`store::insert`，失败显式记录 stderr 日志，不静默吞错
- **图片入库**：`store::insert_image(mime, bytes)`——行先入 `clips`（mime 前缀条目），
  二进制写 `images/{id}.bin` 后回填 `image_path`；等长不同内容的图片因 FNV
  内容指纹不再误判重；重复拷贝仅刷新 ts，文件与关联不变
- **回退**：native 探测失败时 `spawn wl-paste --watch niri-clip store`
- **常驻**：`niri: spawn-at-startup "niri-clip daemon"` 或 systemd user unit
  （二选一，单实例锁兜底）

## 3. Store - SQLite WAL

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;            -- 多进程并发（daemon vs TUI/reload 子进程）
CREATE TABLE clips(
    id PK, hash UNIQUE, text, mime, ts, pinned, size,
    image_path TEXT                   -- v2 新增：图片数据文件关联
);
CREATE INDEX idx_hash ON clips(hash);
CREATE INDEX idx_pinned_ts ON clips(pinned DESC, ts DESC);
-- FTS5 占位表已删除：从未参与查询且需 trigger 才能正确同步；
-- 待 v1.0 应用内搜索功能一并以正确姿势重建
```

- **schema 迁移**：`PRAGMA user_version` 驱动。0→1 建基表并清理 FTS 占位；
  1→2 补 `image_path` 列。此后 schema 变更必须新增版本号与迁移步骤
- **插入原子性**：SELECT 去重检查 + INSERT 包在 `BEGIN IMMEDIATE` 事务里。
  否则多进程并发（典型：fzf 选中旧条目 → wl-copy 写回 → daemon 同时捕获）
  双双通过检查后一方撞 UNIQUE 报错被静默吞掉
- **上限裁剪**：超出 `max_items` 时删除最旧的非 pinned 条目（抽成
  `enforce_max_items`，文本/图片共用）
- **菜单直查**：`list(min(max_items, TUI_LIMIT))`，TUI_LIMIT=300。
  进程内缓存层已移除——fzf 每次 reload-sync spawn 全新 `list-raw` 进程，
  OnceLock 缓存在该路径从未生效；实测 list 300 <11ms 无需缓存

## 4. TUI - 不跳顶

- **后端选择**：`tui_backend=auto` 时**只检测 fzf 是否存在即启用 fzf**
  （fzf 运行于任意终端；kitty/chafa 仅影响图片预览渲染，不参与门控），
  缺失回退 fuzzel。菜单取数统一 `menu_clips() -> "★\tid\tpreview"`
- **fzf**：`fzf --track --id-nth 2 --with-nth 1,3.. --preview 'niri-clip preview {2}' --bind 'ctrl-x:execute-silent(niri-clip delete {2})+reload-sync(niri-clip list-raw)'`
- **fuzzel**：`tui_backend=auto` 无 `fzf` 时 `fuzzel --dmenu`
- **预览**：`preview_id` 判 `mime image/*` → 读取该条目自己的
  `clips.image_path` → `chafa --format symbols --size 60x20`（或 kitty 提示路径）；
  文本则廉价截断（O(width)）+ 单行化，不整串扫描

## 5. 配置

`~/.config/niri-clip/config.toml`，`serde(default)` 容错缺字段。
读取时机：每个子命令入口 / 入库时各读一次（daemon 轮询循环内亦每 tick 读取，
成本为一次小文件 IO）；基于 mtime 监听的真正热重载列入选型 backlog，
当前语义是"改动即刻生效于下一次调用"。

## 6. 打包

- `PKGBUILD` `cargo build --release --locked` → `/usr/bin/niri-clip` + `/usr/share/doc` + `systemd user`
- `PKGBUILD.git` `git+https://...` + `pkgver()`
- `AUR` `makepkg --printsrcinfo > .SRCINFO`

## 7. 与 cliphist 关系

**完全独立**：`v1.0` 不再双写，`cliphist` 仅作为 `migrate` 源。旧版 `Mod+Shift+V` 可保留为 `cliphist` 独立入口，但默认 `niri-clip` 不感知。

## 8. 为什么 Rust + SQLite + fzf

- **Rust**：`<40MB` 常驻，`tokio` 异步，无 `fork`，发 `AUR` 最稳
- **SQLite WAL**：单文件备份，`FTS5` 搜索，`WAL` 读写不锁
- **fzf**：`--track` 是唯一“删除不跳顶”不闪的实现，`ratatui` 自绘后续可选
