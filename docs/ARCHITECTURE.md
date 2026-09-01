# ARCHITECTURE - niri-clip 全新独立设计

> 目标：完全独立于 cliphist 的高性能 Wayland 剪贴板历史

## 1. 总览

```
┌─────────────────────────────────────────────────────────┐
│ niri (Wayland, ext-data-control / wlr-data-control)     │
└────────────────┬────────────────────────────────────────┘
                 │ Wayland 协议
        ┌────────▼──────────┐ daemon.lock 单实例 flock
        │ daemon (Rust)     │ wl-paste --watch 事件源(轮询兜底)
        │ tokio + notify    │──────┘
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

## 2. Daemon - 事件驱动捕获

- **主模式（v0.4.1 起，默认）**：
  `wl-paste --watch sh -c 'exec timeout ${capture_timeout_secs}s niri-clip store'`
  selection 变化时 wl-paste 把载荷直灌子进程 stdin——零空闲往返、无需进程内
  Wayland 会话轮询。每次捕获被 `timeout` 划界（默认 5s，可配置），个别来源应用
  的病态读挂起会被秒级回收。**该结构不存在"daemon 存活但捕获停滞"的形态**
  （issue #2 复盘的根因即纯轮询中 read_to_end 无限阻塞且无错误输出）
- **store 子命令（热点路径）**：stdin 非空且为 UTF-8 → 直接走 `ignore_regex`
  过滤 + 事务化 upsert 入库，全程不触碰本进程 Wayland 连接；
  非 UTF-8 载荷显式忽略并记日志；stdin 为空（手动调用兼容）→ 先 Text 探测，
  再按 `enable_image_preview` 尝试 image/png|jpeg|webp 显式 MIME 抓取
- **回退模式（native 轮询）**：仅当系统缺失 `wl-paste` 二进制时启用。
  单次 get_contents 探测通过（Ok 或 ClipboardEmpty/NoMimeType/NoSeats 三类良性错误）
  后进入 500ms 轮询循环。已知取舍须文档明示：<500ms 连续复制的丢帧窗口、
  空闲 Wayland 往返功耗、read_to_end 长阻塞风险——生产部署应安装 wl-clipboard
- **反模式备忘**：禁止改回"两次调用 + 第二次 unwrap_err()"的探测写法——
  剪贴板恰在两次之间变为可用会 panic，systemd Restart 下表现为周期崩启
- **单实例**：启动即对 `state_dir/daemon.lock` 加 `flock(LOCK_EX|LOCK_NB)`，
  双开报错退出；进程崩溃内核自动释放锁，无陈锁残留
- **常驻托管**：推荐 `niri-clip install-service` 安装内置单元后
  `systemctl --user enable --now niri-clip`；与 niri `spawn-at-startup` 同时配置也安全
  （flock 兜底）。日志经 stderr 进入 journald：`journalctl --user -u niri-clip -f`

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
-- FTS5 全文索引（v3，任务 2.1）：clips_fts 外部内容表（content='clips'，
-- 不复制正文）+ insert/delete/update 三触发器同步，tokenizer=trigram
-- （选型见 ADR-002：中英文子串均命中）
```

- **schema 迁移**：`PRAGMA user_version` 驱动。0→1 建基表并清理 FTS 占位；
  1→2 补 `image_path` 列；2→3 建 clips_fts 全文索引并回填存量。此后 schema 变更必须新增版本号与迁移步骤
- **插入原子性**：SELECT 去重检查 + INSERT 包在 `BEGIN IMMEDIATE` 事务里。
  否则多进程并发（典型：fzf 选中旧条目 → wl-copy 写回 → daemon 同时捕获）
  双双通过检查后一方撞 UNIQUE 报错被静默吞掉
- **上限裁剪**：超出 `max_items` 时删除最旧的非 pinned 条目（抽成
  `enforce_max_items`，文本/图片共用）
- **菜单直查**：`list(min(max_items, TUI_LIMIT))`，TUI_LIMIT=300。
  进程内缓存层已移除——fzf 每次 reload-sync spawn 全新 `list-raw` 进程，
  OnceLock 缓存在该路径从未生效；实测 list 300 <11ms 无需缓存
- **全库搜索（2.1）**：`store::search` ≥3 字符走 `clips_fts MATCH` 短语查询
  + bm25 相关度（FTS5 的 MATCH 左侧必须是 fts 表名本身，别名会被当列名）；
  <3 字符退化为 LIKE 线性扫描（trigram 对短查询无增益，通配符已转义）。
  GUI 搜索经后台线程取候选（(query, gen) 双新鲜度缓存，过期丢弃），再以
  fzf 风格评分重排保持 UX 一致；CLI 暴露 `search` 子命令；fzf TUI 内嵌
  过滤保持 fzf 自身模糊匹配。实测 `fts_search_300_of_10k` ≈0.16ms
  （预算 <50ms）。trigram 按字面索引标点：跨标点短语不命中（子串语义）
  仅 dev 依赖、裁掉 plotters/rayon 特性，闭包 +11 crate）。种子经公开入库
  API 写入临时 XDG 环境（与真实捕获路径同构，schema 演进不破坏基准）。
  实测基线（2026-08-31，本机 10k 条库）：`list_300_of_10k` ≈0.95ms（含
  Config::load + connect 全口径）、`sqlite_select_300_of_10k` ≈0.47ms，
  均远低于 ROADMAP 预算（11ms / 4ms）。运行：`cargo bench -p niri-clip-core`

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

## 9. 依赖与构建开销审计（1.8，2026-09-01）

> 口径：`cargo tree -e normal --prefix none | sort -u | wc -l`（去重 crate 数，
> 不含 dev/bench）；编译时间：`cargo clean` 后全新 `cargo build --release --locked`，
> profile `[lto=true, codegen-units=1, strip=true]`。

**闭包基线（审计 + 两轮收窄后）：**

| 包 | crate 数 | 说明 |
|---|---|---|
| niri-clip（CLI 主包） | 108 | 审计基线 171；notify-send 交换（决策项②）后 -63 |
| niri-clip-core | 88 | 主包子集（基线 150） |
| niri-clip-gui | 247 | 审计前 363：image 收窄 -63、notify-send -53 |
| workspace 总计 | 269 | 审计前 385 |

**大头分解：**

- `notify-rust`（决策项②，2026-09-01 已落地）：原 86 crate 的 zbus/zvariant
  D-Bus 栈为 CLI 主包最大单项。核实实际 API 面仅 summary/body（9 处调用），
  已换 `notify-send` 子进程（`core::notify::send`：后台线程 spawn+wait 收尸，
  调用方零阻塞、daemon 长驻无僵尸；notify-send 缺失静默，与原 `let _ =`
  语义一致；参数数组不经 shell 无注入面；经 coreutils timeout 5s 划界，
  对齐 ROADMAP 工程原则 1——防 D-Bus 异常时 libnotify 默认 ~25s 超时
  导致线程/子进程无界堆积）。代价：新增运行时依赖
  `libnotify`（PKGBUILD*/.SRCINFO.example depends 已同步）；未来若需通知
  action 回调需换回库方案
- `wl-clipboard-rs` → 44（内含 wayland-client 22）：功能必需，无裁剪空间，与
  ROADMAP 预估 ~40 一致
- `iced` → 258：仅 GUI 包引用，不进主包闭包
- `image` → 收窄后小闭包（见下）；`chrono` default 的 oldtime/wasmbind 为
  无操作特性（原生目标零成本），不动

**feature 收窄（本轮唯一落地项）：** iced `image` feature 实为
`image-without-codecs` + `image/default`，会把 avif/exr/gif/tiff/hdr/qoi/pnm/
dds/bmp/tga/ico 全套解码器 + rayon 拉进闭包；而 GUI 解码全部走直接依赖
image 的后台预解码（`load_from_memory` → `Handle::from_rgba`，iced 渲染器
零解码，机制见 v0.5.1 GUI 三轮修复条目）。改为 iced `image-without-codecs` + 直接依赖
`image = default-features=false, features=["png","jpeg","webp"]`：28 个
lockfile 包出图，Cargo.lock -576 行；非 png/jpeg/webp 格式解码失败走既有
优雅降级提示。二进制体积：CLI 6.5 MiB / GUI 11.2 MiB（strip 后；
notify-send 交换后降至 5.1 / 9.8 MiB）。

**编译时间基线（本机，2026-09-01）：** 主包 96s / GUI 增量 123s /
全 workspace ≈219s。原 ROADMAP `<60s` 预算定于 Phase 0 早期依赖树远小
于今日之时（bundled sqlite C 编译 + 全量 LTO 是主要耗时）。
**已决策（2026-09-01）：** 预算重估为 `<120s`，当前达标；`lto = "thin"`
备选搁置（体积/性能回退未测，无实际需求不引入变量）。CI bench 门禁
不含编译时间，无回归报警风险。notify-send 交换后二测：主包 77s /
GUI 增量 108s（原 96s / 123s），预算内余量进一步扩大。
