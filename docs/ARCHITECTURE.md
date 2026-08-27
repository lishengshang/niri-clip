# ARCHITECTURE

## 整体

```
┌─────────────┐     wl_copy      ┌──────────────┐    SQLite WAL    ┌────────────┐
│  App (Ctrl+C)├─────────────────►│   Daemon     ├────────────────►│   Store    │
└─────────────┘                  │ (watcher)    │                 │  + FTS5    │
                                 └──────┬───────┘                 └─────┬──────┘
                                        │ reload-sync                 │
┌─────────────┐   nirius focus   ┌──────▼───────┐   execute-silent  │
│  niri (Mod+V)├───────────────►│  TUI (fzf)   │◄────────────────┘
└─────────────┘                  │ --track      │
                                 │ --id-nth 2   │
                                 └──────────────┘
```

## Daemon (Rust)

- `watcher.rs`: `wl-clipboard-rs` 订阅 `offer`，`hash` 去重后 `store.insert()`
- `filter.rs`: `Regex::new(r"(?i)password|token|secret")`, `min_length`, `mime` 白名单
- `store.rs`: `rusqlite` `WAL` 模式，`insert` 后 `notify::notify` 触发 TUI `reload`

## TUI (v0.1 Bash, v1.0 Rust)

v0.1 复用 `fzf`：
```
build.sh -> fzf --track --id-nth 2
                --bind 'ctrl-x:execute-silent(delete.sh {2})+reload-sync(build.sh)'
                --bind 'ctrl-p:execute-silent(pin.sh {2})+reload-sync(build.sh)'
                --preview 'preview.sh {2}'
```

v1.0 可选 `fzf --listen` HTTP API：Daemon `curl -X POST localhost:$FZF_PORT -d 'reload(...)'` 实现外部剪贴板自动刷新而不重启 TUI。

## Store 演进

v0.1: `cliphist BoltDB` + `pinned.ids` 文件  
v1.0: `~/.cache/niri-clip/db.sqlite` 兼容导入：

```bash
niri-clip migrate --from-cliphist
```

## 配置

`~/.config/niri-clip/config.toml`:

```toml
max_items = 1000
preview_width = 100
ignore_regex = "(?i)password|secret|token"
enable_image = false
```

## 集成 niri

`assets/niri-clip.kdl`:

```kdl
binds {
    Mod+V { spawn "niri-clip" "tui"; }
}
```

`assets/niri-clip.service`:

```ini
[Unit]
Description=niri-clip daemon
PartOf=graphical-session.target

[Service]
ExecStart=%h/.cargo/bin/niri-clip daemon
Restart=on-failure

[Install]
WantedBy=niri.service
```
