# niri-clip

> 为 `niri` 合成器打造的 **全新、高性能、开箱即用** Wayland 剪贴板历史管理器

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Version](https://img.shields.io/badge/version-0.3.0-blue)](Cargo.toml)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange)](https://www.rust-lang.org)
[![Wayland](https://img.shields.io/badge/Wayland-niri-4a90e2)](https://github.com/YaLTeR/niri)
[![AUR](https://img.shields.io/badge/AUR-niri--clip-blue)](https://aur.archlinux.org/packages/niri-clip)

**`niri-clip` 是一个独立的剪贴板历史软件，不是 `cliphist` 的包装。** 它拥有自己的守护进程、数据库和 TUI，专为 `niri` + `Wayland` 优化。

---

### ✨ 特性

- **不跳顶**：`fzf --track --id-nth` 单进程 `reload-sync`，删除/固定后光标停在 **下一个**，删末尾停在 **上一个**
- **高性能**：`Rust + SQLite WAL + FTS5`，`TUI 300` 懒加载 + `200ms` 缓存，`10k` 条 `list <11ms` / `sqlite <4ms`，常驻 `<40MB`
- **原生 Wayland**：`wl-clipboard-rs` 轮询 `500ms`，不 `fork wl-paste`，`ext-data-control` / `wlr-data-control` 自动适配 `niri`
- **图片预览**：`chafa` / `kitty icat`，`enable_image_preview=true` 时 `image/png/jpeg/webp` 终端渲染
- **安全**：`ignore_regex` 默认过滤 `password|secret|token|otp`，`min_store_length` 可配
- **开箱即用**：`paru -S niri-clip` → `Mod+V` 直接可用，`fuzzel` 自动回退无 `kitty` 环境

---

### 📦 安装

#### AUR (推荐)

```bash
paru -S niri-clip          # release
# 或
paru -S niri-clip-git      # git
```

#### Cargo

```bash
cargo install --path . --force  # -> ~/.cargo/bin/niri-clip
# 或从 crates.io (v1.0 后)
cargo install niri-clip
```

#### 手动

```bash
git clone https://github.com/lishengshang/niri-clip
cd niri-clip
cargo build --release
sudo install -Dm755 target/release/niri-clip /usr/bin/niri-clip
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
tui_backend = "auto"  # auto|fzf|fuzzel
enable_preview = true
```

---

### 🏗️ 技术栈

| 层 | 技术 | 说明 |
|---|---|---|
| 语言 | Rust 1.75+, Tokio | 异步 daemon, 无 GC |
| Wayland | wl-clipboard-rs 0.9 | ext-data-control 轮询 |
| 存储 | rusqlite + SQLite WAL + FTS5 | 单文件 `~/.cache/niri-clip/db.sqlite` |
| 去重 | blake3 / DefaultHasher | hash 去重 O(1) |
| TUI | fzf 0.44+ / fuzzel | --track --id-nth 不跳顶 |
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
niri-clip preview <id>
niri-clip pin <id>    # 切换固定
niri-clip delete <id>
niri-clip wipe
niri-clip migrate     # 从 cliphist 导入
niri-clip status
```

---

### 🧪 测试

```bash
./tests/manual.sh              # 20 条 pos 跟随 + 压测
cargo test
cargo bench
```

---

### 🗺️ 路线

- **v0.3 ✅** 原生 daemon + 300 缓存 + chafa
- **v1.0** AUR 正式、waybar、man、CI

---

### 📄 License

MIT © lishengshang
