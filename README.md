# niri-clip

> 为 `niri` 合成器打造的 **全新、高性能、开箱即用** Wayland 剪贴板历史管理器

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.5.0-blue)](Cargo.toml)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange)](https://www.rust-lang.org)
[![Wayland](https://img.shields.io/badge/Wayland-niri-4a90e2)](https://github.com/YaLTeR/niri)
[![AUR](https://img.shields.io/badge/AUR-审核中-blue)](https://aur.archlinux.org/packages/niri-clip)

**`niri-clip` 是一个独立的剪贴板历史软件，不是 `cliphist` 的包装。** 它拥有自己的守护进程、数据库和 TUI，专为 `niri` + `Wayland` 优化。

---

### ✨ 特性

- **不跳顶**：`fzf --track --id-nth` 单进程 `reload-sync`，删除/固定后光标停在 **下一个**，删末尾停在 **上一个**
- **当前项可见**：`▶` 永远指向最后一次复制的内容 ≈ `Ctrl+V` 会粘出的东西，且固定在第 1 行（星标之上）；被安全过滤/超限的内容会显示"当前剪贴板不在历史中"，不撒谎
- **高性能**：`Rust + SQLite WAL`（`busy_timeout=5000` + 事务化去重，多进程并发安全），菜单直查 `300` 条，`10k` 条 `list <11ms` / `sqlite <4ms`
- **事件驱动捕获**：`wl-paste --watch` 主路径，selection 变化才入库、零空闲轮询；每次捕获子进程受 `capture_timeout_secs` 时间边界保护，从机制上杜绝"进程活着但捕获停摆"；原生 500ms 轮询仅为无 wl-paste 环境兜底
- **数据持久安全**：历史库位于 `~/.local/state/niri-clip/`（XDG state 规范，不会被系统清理工具误删），旧 `~/.cache` 库自动快照搬迁；目录 0700 / 库文件 0600 权限收紧
- **图片预览**：`chafa` / `kitty icat`，`enable_image_preview=true` 时 `image/png/jpeg/webp` 终端渲染
- **安全**：`ignore_regex` 默认过滤 `password|secret|token|otp`，`min_store_length` 可配
- **开箱即用**：装好即 `Mod+V` 直接可用（AUR 上架前用 `cargo install` / `makepkg`），`fuzzel` 自动回退无 `fzf` 环境

---

### 📦 安装

#### AUR（即将上架，审核中）

#### Cargo

```bash
cargo install niri-clip
# 原生 GUI（可选，需单独安装）
cargo install niri-clip-gui
```

#### 源码 / makepkg

```bash
git clone https://github.com/lishengshang/niri-clip
cd niri-clip
makepkg -si          # Arch 打包（含 systemd 单元/配置示例）
# 或
cargo build --release -p niri-clip && sudo install -Dm755 target/release/niri-clip /usr/bin/niri-clip
```

---

### 🚀 快速开始

```bash
# 1. 配置（可选）
cat ~/.config/niri-clip/config.toml
# max_items=750 preview_width=100 tui_backend=auto enable_image_preview=true

# 2. 启动守护进程（niri 已自动 spawn-at-startup，无需手动）
niri-clip daemon &

# 3. 状态
niri-clip status

# 4. 一次性从 cliphist 迁移（可选，之后独立）
niri-clip migrate

# 5. 打开 TUI
niri-clip tui  # 等价 Mod+V
```

**niri 集成** `~/.config/niri/config.kdl`：

```kdl
// 已自动
spawn-at-startup "niri-clip daemon"
binds {
    Mod+V { spawn "niri-clip" "tui"; }
}
```

---

### ⚙️ 配置 `~/.config/niri-clip/config.toml`

```toml
max_items = 750
preview_width = 100
min_store_length = 1
enable_image_preview = true   # chafa
ignore_regex = "(?i)password|secret|token|otp|auth"
pinned_on_top = true
tui_backend = "auto"  # auto|native|fzf|fuzzel（native=无终端原生窗口）
notify_enabled = true # v0.5 桌面通知开关（false 完全静默）
enable_preview = true
capture_timeout_secs = 5   # v0.4.1 每次捕获子进程超时
capture_primary = false    # v0.5.2 PRIMARY 选中即捕获（划选入库，中键粘贴语义）
max_clip_bytes = 1048576   # v0.5 单条文本上限（字节），超限拒绝入库
max_image_bytes = 10485760 # v0.5 单张图片上限（字节）
max_image_total_bytes = 209715200 # v0.5.1 images/ 总量配额（字节），超限 LRU 淘汰，0 不限
```

---

### 🏗️ 技术栈

| 层 | 技术 | 说明 |
|---|---|---|
| 语言 | Rust 1.75+, Tokio | 异步 daemon, 无 GC |
| Wayland | wl-paste --watch 事件源 + wl-clipboard-rs | 变化即捕获；轮询仅兜底 |
| 存储 | rusqlite + SQLite WAL | 单文件 `~/.local/state/niri-clip/db.sqlite`，`PRAGMA user_version` 版本化迁移；FTS5 全文索引（trigram，中英文子串搜索，见 ADR-002） |
| 去重 | 文本 DefaultHasher+len / 图片 FNV1a64+mime+len | 图片指纹跨进程稳定；文本 hash 计划随 v1.0 统一到稳定算法 |
| TUI | fzf 0.71+ / fuzzel | --track --id-nth 不跳顶；低于 0.71 自动回退 fuzzel |
| 预览 | chafa, kitty icat | 图片终端渲染 |
| 打包 | PKGBUILD, systemd user | AUR |

详见 `docs/ARCHITECTURE.md`。

---

### 📖 命令

```
niri-clip daemon      # 后台监听
niri-clip tui         # 打开历史 (Mod+V)
niri-clip store       # 从 stdin 入库 (供 wl-paste --watch)
niri-clip list-raw    # 供 fzf reload
niri-clip search <query> [--limit N]
                      # 全库全文搜索（FTS5 trigram，中英文子串；≥3 字符
                      # 走索引，更短退化为 LIKE）
niri-clip preview <id>
niri-clip pin <id>    # 切换固定
niri-clip delete <id> [-f]   # -f 跳过星标确认（脚本/无头环境）
niri-clip wipe
niri-clip migrate     # 从 cliphist 导入
niri-clip install-service
                      # 一键安装 systemd user 单元
niri-clip status
```

### 🖥️ 原生 GUI（`tui_backend = native` / `auto`）

`niri-clip tui` 检测到 `niri-clip-gui` 时打开原生窗口：无终端、秒开、
winit 原生 IME（中文搜索直打）、单实例（Mod+V 连按聚焦已开窗口）。

| 按键 / 鼠标 | 作用 |
|---|---|
| ↑ / ↓ | 移动选中（列表自动滚动跟随） |
| Enter / 左键点击行 | 复制并关闭 |
| Ctrl-Y / 右键点击行 | 连续复制（不退出） |
| Ctrl-P | 固定 / 取消固定 |
| Ctrl-X | 删除（星标条目二段确认） |
| 1-9,0 | 快选第 1-10 行（空查询时） |
| Alt+1-9,0 | 快选第 1-10 行（任意时刻可用，含搜索态） |
| 直接输入 | fzf 式子序列匹配 + 相关度排序（覆盖全库，含中文） |
| Esc | 清除查询；空查询退出 |

窗口打开位置/边框/阴影由 niri window-rule 控制，示例见
`assets/niri-clip.kdl`（顶部浮动 + 无焦点环/阴影）。

### 🧩 systemd 托管（推荐）

```bash
niri-clip install-service
systemctl --user daemon-reload
systemctl --user enable --now niri-clip.service
journalctl --user -u niri-clip -f      # 日志
```

---

### 🧪 测试

```bash
./tests/manual.sh              # 20 条 pos 跟随 + 压测
cargo test                     # 单元测试（XDG 隔离环境）
cargo clippy --all-targets     # lint 门禁（零警告基线）
```

---

### 🗺️ 路线

- **v0.3 ✅** 原生 daemon + 300 缓存 + chafa
- **v0.4.x ✅** P0 修复（并发/图片/panic/state 迁移）+ 事件驱动捕获 + systemd 托管 + CI
- **v0.5 ▶** TUI 体验闭环（PRIMARY selection / 配额 GC / 基准进 CI / man）
- **v0.6** FTS5 全文搜索 + 数据治理（稳定 hash / GC / 导出）
- **v0.7** 安全与隐私强化（过滤规则 / systemd 沙箱 / 加密 PoC）
- **v1.0** Production GA：AUR 三包、crates.io、waybar、CI 深化

完整阶段规划见 [docs/ROADMAP.md](docs/ROADMAP.md)。

---

### 📄 License

MIT © lishengshang
