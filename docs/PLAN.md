# niri-clip PLAN - 全新独立软件

## 0. 定位

**`niri-clip` 是一个全新的 Wayland 剪贴板历史软件，为 `niri` 而生，完全独立于 `cliphist`。**

- 单一真相源 `~/.cache/niri-clip/db.sqlite`
- 开箱即用 `paru -S niri-clip` → `Mod+V`
- 高性能 `Rust + SQLite WAL + fzf`，常驻 40MB，10k 搜索 <50ms

## 1. 为什么全新

`cliphist` (BoltDB + `wl-paste --watch`) 在 `niri` 下有 3 痛：
1. `while true; fzf --expect` 重启跳顶
2. `BoltDB` 无 `FTS`，搜索慢
3. `wl-paste fork` 额外进程

`niri-clip` 重写解决，不兼容 `cliphist` 运行时，仅 `migrate` 一次性导入。

## 2. 技术选型 (v1.0 目标)

| 层 | 选型 | 原因 |
|---|---|---|
| 语言 | Rust + Tokio | 无 GC，异步 daemon |
| Wayland | wl-clipboard-rs 0.9 | 原生 ext-data-control 轮询 |
| 存储 | rusqlite WAL（FTS5 待 v1.0 搜索） | 单文件，WAL 不锁 |
| TUI | fzf --track | 不跳顶唯一解 |
| 预览 | chafa | 图片终端 |

## 3. 数据模型 (v0.4)

```sql
clips(id PK, hash UNIQUE, text, mime, ts, pinned, size, image_path)
idx_hash, idx_pinned_ts
```

hash 去重（事务化原子 upsert），pinned DESC/ ts DESC 排序，
菜单直查 min(max_items,300)，无缓存层；schema 由 PRAGMA user_version 版本化迁移。

## 4. 功能 (v1.0 独立)

- `daemon` 原生 500ms 轮询 + flock 单实例 + 探测单次化，双写已移除
- `store` 双写已移除，`wipe` 不清 cliphist，多进程并发安全（busy_timeout）
- `tui` 直查 menu_clips，Mod+V 唯一入口，图片预览按 clip id 关联
- `migrate` 仍保留，之后可删

## 5. 性能

`list 300 <11ms`, `sqlite 10k <4ms`; 大文本 reload 走 O(width) 廉价截断。

## 6. 发布

`GitHub: lishengshang/niri-clip` `AUR: niri-clip / niri-clip-git` `cargo install niri-clip`

## 7. 下一步

`v0.3` 已交付，`v1.0` 做 `waybar + man + CI`，`Mod+Shift+V` 旧版下一版移除。
