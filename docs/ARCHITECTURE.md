# ARCHITECTURE - niri-clip 全新独立设计

> 目标：完全独立于 cliphist 的高性能 Wayland 剪贴板历史

## 1. 总览

```
┌─────────────────────────────────────────────────────────┐
│ niri (Wayland, ext-data-control / wlr-data-control)     │
└────────────────┬────────────────────────────────────────┘
                 │ Wayland 协议
        ┌────────▼────────┐
        │ daemon (Rust)   │  wl-clipboard-rs 500ms 轮询
        │ tokio + notify  │──────┐
        └────────┬────────┘      │ hash(ignore_regex) dedup
                 │ insert        ▼
        ┌────────▼────────┐  ┌─────────────┐
        │ store (SQLite)  │  │ FTS5        │
        │ WAL  clips      │◄─┤ clips_fts   │
        │ idx_hash, idx_pinned_ts│         │
        └────────┬────────┘  └─────────────┘
                 │ list_tui 300 + 200ms cache (OnceLock)
        ┌────────▼────────┐  --track --id-nth 2
        │ tui (fzf)       │  execute-silent + reload-sync
        │ fuzzel 回退     │  chafa 预览
        └────────┬────────┘
                 │ copy (wl-clipboard-rs)
        ┌────────▼────────┐
        │ Wayland clipboard│
        └─────────────────┘
```

**独立性：** 单一真相源 `~/.cache/niri-clip/db.sqlite`，不读写 `~/.cache/cliphist/db`，`migrate` 仅一次性导入。

## 2. Daemon - 原生 Wayland

- **轮询**：`paste::get_contents(Regular, Unspecified, Text)` 每 500ms，`ClipboardEmpty/NoMimeType` 忽略
- **去重**：`hash(len+DefaultHasher(text))` 与 `last_hash` 比对，变化才 `store::insert`
- **图片**：若 `enable_image_preview`，尝 `image/png/jpeg/webp` → `placeholder + blob` 缓存到 `~/.cache/niri-clip/images/{ts}.bin`
- **回退**：若 `native` 探测失败 (`NoSeats` 外错误)，`spawn wl-paste --watch niri-clip store`
- **常驻**：`niri: spawn-at-startup "niri-clip daemon"` + `systemd user: WantedBy=niri.service`

## 3. Store - SQLite WAL

```sql
PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
CREATE TABLE clips(id PK, hash UNIQUE, text, blob, mime, ts, pinned, size);
CREATE INDEX idx_hash ON clips(hash);
CREATE INDEX idx_pinned_ts ON clips(pinned DESC, ts DESC);
CREATE VIRTUAL TABLE clips_fts USING fts5(text, content='clips');
```

- **插入**：`ignore_regex` 过滤 → `hash` 查重 → `INSERT + FTS` → `invalidate_cache()` → `count>max_items` 删最旧 `pinned=0`
- **TUI**：`TUI_LIMIT=300`，`list_tui()` 用 `OnceLock<Mutex<CachedList>>` 200ms 缓存，`bench_10k` 10k <50ms
- **固定**：`toggle_pin` 翻转 `pinned` + `invalidate_cache`

## 4. TUI - 不跳顶

- **fzf**：`list_tui() -> "★\tid\tpreview"` → `fzf --track --id-nth 2 --with-nth 1,3.. --preview 'niri-clip preview {2}' --bind 'ctrl-x:execute-silent(niri-clip delete {2})+reload-sync(niri-clip list-raw)'`
- **fuzzel**：`tui_backend=auto` 无 `kitty/fzf` 时 `fuzzel --dmenu`
- **预览**：`preview_id` 判 `mime image/*` → `chafa --format symbols --size 60x20` 或 `kitty icat`，文本则截断 100 行

## 5. 配置

`~/.config/niri-clip/config.toml` `serde(default)` 热重载，每次 `load()` 读，缺字段回退 `Default`。

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
