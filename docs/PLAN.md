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
| 存储 | rusqlite WAL + FTS5 | 单文件，WAL 不锁 |
| TUI | fzf --track | 不跳顶唯一解 |
| 预览 | chafa | 图片终端 |

## 3. 数据模型 (v1.0)

```sql
clips(id PK, hash UNIQUE, text, blob, mime, ts, pinned, size)
idx_hash, idx_pinned_ts, clips_fts
```

`hash` 去重，`pinned DESC, ts DESC` 排序，`TUI_LIMIT 300` + 200ms 缓存。

## 4. 功能 (v1.0 独立)

- `daemon` 原生 500ms 轮询，双写已移除
- `store` 双写已移除，`wipe` 不清 cliphist
- `tui` 独立 `list_tui`，`Mod+V` 唯一入口
- `migrate` 仍保留，之后可删

## 5. 性能

`list 300 <11ms`, `sqlite 10k <4ms`, `daemon <1% CPU`

## 6. 发布

`GitHub: lishengshang/niri-clip` `AUR: niri-clip / niri-clip-git` `cargo install niri-clip`

## 7. 下一步

`v0.3` 已交付，`v1.0` 做 `waybar + man + CI`，`Mod+Shift+V` 旧版下一版移除。
