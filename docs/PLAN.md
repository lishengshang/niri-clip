# niri-clip 项目规划 V1

## 0. 项目定位

**名字：** `niri-clip` (主) / `clipnova` (别名)  
**一句话：** 为 `niri + Wayland` 生的开箱即用、高性能剪贴板历史。

**核心价值：**
- 开箱即用：`paru -S niri-clip` + `Mod+V` 即可，不配 `kitty/fzf` 也能跑 (fuzzel fallback)
- 高性能：Daemon 常驻，SQLite WAL，10k条搜索 <50ms，内存 <40MB，启动 <120ms
- 不跳顶：删除/固定后光标停在 下一个/上一个，符合所有现代剪贴板 (CopyQ, GPaste) 心智
- Wayland 原生：`wl-clipboard-rs`，尊重 `disable-primary`，不存密码

## 1. 现状与根因

### 1.1 当前链路 (bash)
```
binds.kdl Mod+V -> clipboard-history-ui.sh -> nirius focus-or-spawn -> kitty -> clipboard-history-tui.sh
                                                                                  -> while true; build_menu | fzf --expect
daemon: clipboard-history.sh -> wl-paste --watch cliphist store (BoltDB)
```

### 1.2 为什么会跳顶
1. `while true` 每次 `continue` 都重启 `fzf` 进程，默认 `pos(1)`
2. 未使用 `fzf --track --id-nth --bind reload` 原地刷新能力
3. `build_menu` 两次 `cliphist list`，删除后列表缩短但无位置记忆

## 2. 技术选型

### 2.1 v0.1 Bash Plan B (已交付)
- 保持 `cliphist` BoltDB 兼容，降低迁移风险
- 单 `fzf` 进程：`build_menu | fzf --track --id-nth 2 --bind 'ctrl-x:execute-silent+reload-sync'`
- 生成临时脚本 `~/.cache/niri-clip/{build-menu,toggle-pin,delete,wipe,preview}.sh` 供 `reload` 调用
- `pinned.ids` 仍文件存储，后续迁 DB `pinned` 列

**优点：** 无新依赖，10分钟验证  
**缺点：** 仍 fork `cliphist`, 有 50ms 闪

### 2.2 v1.0 Rust (目标)
- `wl-clipboard-rs` 直接监听 `wl_seat`, 不依赖 `wl-paste` 子进程
- `rusqlite` WAL + `FTS5` 全文索引，`hash` 去重 O(1)
- `tokio` 异步 Daemon，`notify-rust` 通知
- TUI 二选一：a) 复用 `fzf --listen` HTTP API (最快) b) `ratatui` 自绘 (最可控)

| 维度 | Bash v0.1 | Rust v1.0 |
|------|-----------|-----------|
| 启动 | 180ms | 60ms |
| 搜索 5k | 200ms | 30ms |
| 内存 | 20MB+fzf | 35MB |
| 图片 | 不支持 | 支持 (chafa) |

## 3. 数据模型

### 3.1 v0.1 (兼容)
```
cliphist BoltDB: id -> {text, mime, time}
pinned.ids: 每行一个 id
```

### 3.2 v1.0 (SQLite)
```sql
CREATE TABLE clips (
  id INTEGER PRIMARY KEY,
  hash TEXT UNIQUE,          -- blake3(text)
  text TEXT,                 -- 截断 100KB, 超大存 blob
  blob BLOB,                 -- 原始二进制 (可选)
  mime TEXT DEFAULT 'text/plain',
  ts INTEGER,                -- unix ms
  pinned INTEGER DEFAULT 0,
  size INTEGER
);
CREATE VIRTUAL TABLE clips_fts USING fts5(text, content='clips');
CREATE INDEX idx_hash ON clips(hash);
CREATE INDEX idx_pinned_ts ON clips(pinned DESC, ts DESC);
```

## 4. 功能清单

### v0.1 MVP (已完成)
- [x] 单进程 fzf，删除不跳顶
- [x] ★ 置顶/取消，保序
- [x] 预览 `down:5` 全量 decode
- [x] `Ctrl+P/X/R`, `Alt+X` 全部 `reload-sync`
- [x] 星标删除二次确认 (fuzzel)

### v0.2 体验 (1周)
- [ ] `config.toml`：`max_items=750`, `preview_width`, `ignore_regex="password|secret"`
- [ ] 图片预览：`kitty icat` 探测 `image/png`
- [ ] 性能：`cliphist list` -> `limit 300` 懒加载
- [ ] 安全：`1Password` 复制自动忽略
- [ ] `fuzzel` fallback：无 `kitty` 时纯 Wayland 弹窗

### v1.0 高性能 (2-3周)
- [ ] Daemon：`wl-clipboard-rs` + `tokio` + `SQLite`
- [ ] 去重：`hash` 查重，`max_dedupe_search` 废弃
- [ ] FTS5：`niri-clip search "rust | wayland"`
- [ ] CLI：`niri-clip {daemon,tui,wipe,search,export}`

### v1.5 生态
- [ ] AUR `niri-clip-bin` + `niri-clip-git`
- [ ] `waybar` 模块
- [ ] `SSH OSC52` 远程同步
- [ ] `niri` overview 缩略图

## 5. 性能目标

| 指标 | 目标 | 测量 |
|------|------|------|
| 冷启动 TUI | <120ms | `hyperfine "niri-clip tui"` |
| 搜索 10k | <50ms | `cargo bench` |
| Daemon 内存 | <40MB | `ps -o rss` |
| 大文本 1MB | 不卡 | 截断存储 + 懒 decode |

## 6. 研发流程

1.  **Git**：`main` 保护，`feat/xxx` 分支，`cargo fmt + clippy` CI
2.  **测试**：`cargo test` + `bash` 集成测试 `tests/tui.sh` 模拟 `cliphist store` 20条后验证光标位置
3.  **发布**：`cargo release` + `PKGBUILD` + `gh release` 二进制
4.  **文档**：`README` 中英双语，`man niri-clip`

## 7. 风险

- `wl-clipboard-rs` 在 `niri` 下的 `ext-data-control` 权限：需跟进 `niri` 0.1.10+ 的 `clipboard` 协议
- `fzf --track` 在 `reverse` 布局下的 `pos` 语义：已验证 `id-nth` 方式兼容
- `cliphist` BoltDB 迁移到 SQLite：提供 `niri-clip migrate` 一键导入

## 8. 下一步 (本周)

1.  验证 v0.1 在 lishengshang 机器：`Mod+V -> 删 5次 -> 光标是否跟随`
2.  收集反馈，调 `preview` 高度/`header` 文案
3.  开 `feat/rust-daemon` 分支，搭 `tokio + rusqlite` 骨架
