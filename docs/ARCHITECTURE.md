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

**`state/` 下的全部文件（"无隐藏状态"原则的实际口径）：**

| 文件 | 作用 | 可删除性 |
|---|---|---|
| `db.sqlite` (+`-wal`/`-shm`) | 主库 | 否（数据本体） |
| `images/{id}.bin` | 图片载荷，按 clip id 关联 | 否（不可再生） |
| `current` | ▶ 当前项指针（最后成功捕获的 hash） | 可（退化为无 ▶ 标记） |
| `daemon.lock` | daemon 单实例 flock（`core::single_instance`） | 可（内核释放，无陈锁） |
| `gui.lock` | 原生 UI 单实例 flock（同一机制，2.6/C4 起不再用 PID 文件） | 可（内核释放） |
| `pending_delete` | ★ 条目二段确认挂起态（id + 时间戳，15s TTL）——语义与判定归 `core::confirm`（ADR-005）；fzf TUI 用行尾标记呈现、原生 UI 用横幅、CLI 用提示文案 | 可（TTL 到期或显式 `clear()` 即清） |
| `fzf.version` | fzf 版本门控缓存（二进制 mtime 变化即失效） | 可（退化为每次实查） |
| `tui.log` | 无 TTY 时外层终端重跑的日志（排障入口） | 可 |

**两条并存的 UI 路径**：fzf TUI（经终端，`tui_backend` 为 auto/fzf/fuzzel）
与原生窗口（`niri-clip-gui`，iced xdg 窗口）。二者的**业务语义全部收敛在
core**，UI 只做渲染与输入分发；原生 UI 的完整原理见 §10。

**分层边界**：`niri-clip-core` 是**纯逻辑库**（`confirm` / `config` / `daemon` /
`migrate` / `notify` / `preview` / `single_instance` / `store`），**不含 UI 后端选择
与终端编排**——fzf/fuzzel 的进程编排、终端探测、fzf 版本门控住在
`crates/niri-clip/src/tui.rs`（CLI 职责），原生 UI 不为这些代码买单。
（任务 2.6 / D1 已把 `tui` 从 core 迁出，此前的分层债已还清。）

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
-- 文本 hash = blake3(text) 64 字符 hex（v4，任务 2.2，ADR-003）：
-- DefaultHasher 跨编译器不稳定，存量行 v3→4 一次性重算合并；
-- 图片 hash 仍为 img: 前缀 FNV-64 指纹（image_content_key，本就稳定）
```

- **schema 迁移**：`PRAGMA user_version` 驱动。0→1 建基表并清理 FTS 占位；
  1→2 补 `image_path` 列；2→3 建 clips_fts 全文索引并回填存量；3→4 文本 hash
  全表重算为 blake3（事务内合并重复 + 前置 VACUUM INTO 快照 + ▶ 指针重映射，
  见 ADR-003）。此后 schema 变更必须新增版本号与迁移步骤
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
  过滤保持 fzf 自身模糊匹配。trigram 按字面索引标点：跨标点短语不命中
  （子串语义，非词边界），单测锁定
- **基准设施（1.6）**：`criterion` 为 core 的 **dev-dependency**，只在 dev 闭包
  中出现，不进 `-e normal` 口径（这也是"normal 闭包数"与"编译期依赖数"
  两个概念的分界）。种子经公开入库 API 写入临时 XDG 环境（与真实捕获路径
  同构，schema 演进不破坏基准）。实测基线（2026-08-31，本机 10k 条库）：
  `list_300_of_10k` ≈0.95ms（含 Config::load + connect 全口径）、
  `sqlite_select_300_of_10k` ≈0.47ms、`fts_search_300_of_10k` ≈0.16ms，
  均远低于 ROADMAP 预算（11ms / 4ms / 50ms）。运行：`cargo bench -p niri-clip-core`

- **导出/回灌（2.4，backup.rs）**：NDJSON 流式格式（首行 header + 每行一条目，
  图片内嵌 base64，选型与被拒备选见 ADR-004）。导出读事务（deferred 快照）
  保证 COUNT 与全表扫描一致，文件 0600；回灌 hash 幂等（blake3 / `img:` FNV
  重算比对拦截损坏条目）+ 单 BEGIN IMMEDIATE 事务原子提交，保留原 ts/pinned
  不触碰 ▶ 指针，结束 enforce_max_items；`--sqlite` 走 `VACUUM INTO` 物理快照
  （复用 migrate_legacy_db 模式，目标已存在拒绝覆盖）

## 4. UI 后端 - 不跳顶（本节述 fzf/fuzzel 路径；原生窗口见 §10）

- **后端选择**：`tui_backend` ∈ `auto | native | fzf | fuzzel`。
  `auto` 的优先级是 **native → fzf → fuzzel**：`gui_binary()` 能定位到
  `niri-clip-gui` 即用原生窗口（见 §10），否则看 `fzf` 是否存在
  （fzf 运行于任意终端；kitty/chafa 仅影响图片预览渲染，不参与门控），
  再退 fuzzel。**注意**：是否存在 fzf 只是"存在性"检查，版本地板
  （`--id-nth` 需 ≥0.71）在 `tui::run()` 内二次校验，不达标再退 fuzzel
- **菜单取数**：统一 `menu_clips() -> Vec<Clip>`，行格式为 **5 列**
  `num \t ▶ \t ★ \t id \t preview`（列序即下方 fzf 参数的下标依据）
- **fzf**：`fzf --delimiter='\t' --nth=5.. --with-nth=1,2,3,5.. --id-nth=4
  --track --no-input --no-sort --preview='niri-clip preview {4}'`；
  绑定 `ctrl-p:pin{4}` / `ctrl-x:delete --fzf {4}` / `ctrl-y:copy {4}` /
  `ctrl-r:list-raw` / `alt-1..9:pos(n)+accept`，各动作后接 `reload-sync`
  （`--track` + `--id-nth` 是"删除/固定后不跳顶"的实现基础）；
  匹配只作用于第 5 列，隐藏的序号/标记/id 列不参与搜索（否则查询 `1`
  会命中所有含 1 的序号与 id）
- **fuzzel**：无 fzf（或版本不达标）时 `fuzzel --dmenu`，选中后复制即退出
  （无原地 reload，属最小可用兜底）
- **预览**：`preview_id` 判 `mime image/*` → 读取该条目自己的
  `clips.image_path` → `chafa --format symbols --size 60x20`（或 kitty 提示路径）；
  文本则逐行输出（截 100 行 / 每行 300 字符），发生截断时如实提示实际字节数

## 5. 配置

`~/.config/niri-clip/config.toml`，`serde(default)` 容错缺字段；缺省值定义在
`crates/niri-clip-core/src/config.rs`，注释版样例见 `config/config.toml.example`
（任务 2.6：默认值目前在 `Default::default()` 与 serde 的 `default_*()` 两处
重复硬编码，将收敛为单一来源 + 一致性单测）。

读取时机：每个子命令入口 / 入库时各读一次（daemon 轮询循环内亦每 tick 读取，
成本为一次小文件 IO）；基于 mtime 监听的真正热重载列入选型 backlog，
当前语义是"改动即刻生效于下一次调用"。

`tui_backend` 四态：`auto`（native→fzf→fuzzel）/ `native` / `fzf` / `fuzzel`。
后端选择逻辑见 §4 与 §10。

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

## 9. 依赖与构建开销审计（1.8，2026-09-01；**口径修正 2026-09-10**）

> **口径（唯一权威、可复现）**——由任务 2.6 修正：
>
> ```
> cargo tree -e normal -p <包名> --prefix none | sed 's/ (\*)$//' | sort -u | wc -l
> ```
>
> `cargo tree` 会给"在树中出现多次"的 crate 行加 `(*)` 后缀。**必须先剥除该
> 后缀再 `sort -u`**：否则 `serde v1.0.229` 与 `serde v1.0.229 (*)` 被当成
> 两条独立记录而虚高。本机伪影规模实测：CLI **37** 处 / core 29 处 / GUI 156 处。
> 旧口径漏掉这一步，正是"同一轮审计出现两套数字（112 vs 108、250 vs 247）"
> 的根因——旧口径复测值为 CLI 117 / core 96 / GUI 252 / ws 275，**均偏高**。
> 编译时间：`cargo clean` 后全新 `cargo build --release --locked`，
> profile `[lto=true, codegen-units=1, strip=true]`。

**闭包基线（口径修正后，2026-09-10 实测）：**

| 包 | crate 数 | 说明 |
|---|---|---|
| niri-clip（CLI 主包） | 100 | 旧口径 117；审计前基线 171 |
| niri-clip-core | 83 | 主包子集（旧口径 96）。`criterion` 属 dev-dependency，**不计入**（见 §3 基准设施） |
| niri-clip-gui | 204 | 旧口径 252；审计前 363（image 收窄、notify-send 交换后） |
| workspace 总计 | 222 | 旧口径 275；审计前 385 |

> 该表是**全项目唯一的依赖数真相源**：ROADMAP 开销预算表只记预算与达标状态，
> 不重复记数（2.6 收敛，此前两处各记一套数字）。

**大头分解：**

- `notify-rust`（决策项②，2026-09-01 已落地，**该依赖树已整体出图**）：原
  zbus/zvariant D-Bus 栈为 CLI 主包最大单项（旧口径计 86 crate）。核实实际
  API 面仅 summary/body（9 处调用），已换 `notify-send` 子进程
  （`core::notify::send`：后台线程 spawn+wait 收尸，调用方零阻塞、daemon
  长驻无僵尸；notify-send 缺失静默，与原 `let _ =` 语义一致；参数数组不经
  shell 无注入面；经 coreutils timeout 5s 划界，对齐 ROADMAP 工程原则 1——
  防 D-Bus 异常时 libnotify 默认 ~25s 超时导致线程/子进程无界堆积）。代价：
  新增运行时依赖 `libnotify`（PKGBUILD* depends 已同步）。
  **收益不再以"减少 N crate"表述**：旧文档在 ARCHITECTURE `-63`、`notify.rs`
  `-81`、ROADMAP `86` 三处各写一个数，口径不明（含 `(*)` 伪影），
  本轮统一为事实陈述"该依赖树已完全移除"，不再记差值。未来若需通知 action
  回调需换回库方案
- `wl-clipboard-rs` → 44（内含 wayland-client 22）：功能必需，无裁剪空间，与
  ROADMAP 预估 ~40 一致
- `blake3`（2.2，2026-09-01）：default 仅 std，纯 Rust + 运行时 SIMD 检测，
  **新增 3 个 crate**（blake3/constant_time_eq/cpufeatures），零构建链新增。
  bench 无回归（list_300_of_10k 1.02ms，预算 11ms；hash 在捕获热路径，
  blake3 吞吐 GB/s 级）
- `iced`：仅 GUI 包引用，不进主包闭包（收窄后 GUI 全包 204）
- `image` → 收窄后小闭包（见下）；`chrono` default 的 oldtime/wasmbind 为
  无操作特性（原生目标零成本），不动

**feature 收窄：** iced `image` feature 实为
`image-without-codecs` + `image/default`，会把 avif/exr/gif/tiff/hdr/qoi/pnm/
dds/bmp/tga/ico 全套解码器 + rayon 拉进闭包；而 GUI 解码全部走直接依赖
image 的后台预解码（`load_from_memory` → `Handle::from_rgba`，iced 渲染器
零解码，机制见 v0.5.1 GUI 三轮修复条目）。改为 iced `image-without-codecs` + 直接依赖
`image = default-features=false, features=["png","jpeg","webp"]`：28 个
lockfile 包出图，Cargo.lock -576 行；非 png/jpeg/webp 格式解码失败走既有
优雅降级提示。二进制体积：CLI 6.5 MiB / GUI 11.2 MiB（strip 后；
notify-send 交换后降至 5.1 / 9.8 MiB；2.4 引入 serde_json 序列化面后
CLI 5.5 MiB，增量 0.4 MiB 为 serde_json+base64 代码与元数据）。

**编译时间基线（本机，2026-09-01 测量轮次）：** 主包 96s / GUI 增量 123s /
全 workspace ≈219s。原 ROADMAP `<60s` 预算定于 Phase 0 早期依赖树远小
于今日之时（bundled sqlite C 编译 + 全量 LTO 是主要耗时）。
**已决策（2026-09-01）：** 预算重估为 `<120s`，当前达标；`lto = "thin"`
备选搁置（体积/性能回退未测，无实际需求不引入变量）。CI bench 门禁
不含编译时间，无回归报警风险。notify-send 交换后二测：主包 77s /
GUI 增量 108s（原 96s / 123s），预算内余量进一步扩大。
> 口径说明：编译时间为**单一测量轮次的历史记录**，不随依赖微调重测；
> 若需刷新，须在同一机器、`cargo clean` 后整体重测并替换本段，
> 不得只改动其中一个数字。

## 10. 原生 UI - niri-clip-gui（Phase 5 已交付）

> 本节是原生 UI 的**唯一原理真相源**（原 `docs/NATIVE-UI.md` 立项详案已于
> 任务 2.6 删除，内容全部收敛至此与 ADR-001）。

**定位**：消除终端冷启动瓶颈（ghostty 冷启 ~150ms+，fzf 依赖 TTY 无法绕开）。
目标 Mod+V 到窗口可交互 ≤50ms、零终端依赖、交互语义与 fzf 版 100% 对齐。

**技术栈与两次推翻（ADR-001 修订 1）**：初版选 layer-shell，真机反馈后改为
**常规 xdg 窗口**（app-id `niri-clip-gui`）。两个动因：① layer-shell 无法被
niri `window-rule` 约束（用户无法自定义悬浮/位置/边框）；② layer-shell 的
daemon 式事件循环缺 IME 接线，中文搜索不可用。渲染器由 wgpu/GL 改为
**tiny-skia 纯软件渲染**（NVIDIA 下 wgpu 冻结、EGL 初始化失败；列表 UI 无
GPU 需求，二进制 -1/3，与显卡驱动彻底解耦）。winit 原生 IME 解锁中文输入；
悬浮/位置/阴影由用户 `rule.kdl` 约定（示例见 `assets/niri-clip.kdl`）。

**进程形态**：**按需进程**——`niri-clip tui` 探测到 `niri-clip-gui` 即 spawn
后自身返回；窗口关闭即进程退出，常驻增量归零（不做常驻 UI 服务）。

**后端选择（`tui_backend`）**：`auto` 优先 native → fzf → fuzzel；
`gui_binary()` 三级兜底定位二进制（`PATH` → 与 CLI 同目录 → `~/.cargo/bin`），
因 niri `spawn` 环境的 PATH 常缺 `~/.cargo/bin`（真机踩坑，否则 auto 会误降级）。
显式 `"native"` 但二进制缺失时降级 fzf/fuzzel。fzf 版本地板 0.71
（`--id-nth`）在 `run()` 内二次校验。

**单实例与聚焦**：`state/gui.lock` 存 PID，活实例存在则经 niri IPC
（`niri msg -j windows`，按 app_id 精确匹配）聚焦已开窗口后本进程退出；
残留死锁经 `/proc/{pid}/cmdline` 的 argv[0] 精确复核后覆写接管。
（注：这与 core 的 `daemon.lock` flock 是**两套机制**，收敛计划见任务 2.6。）

**渲染与图片链路**：行列表用 core 的 `preview::preview_text`（单行截断）；
底部预览窗格另做多行截断。图片解码**全部在后台线程**完成
（`load_from_memory` → `Handle::from_rgba`），view 只读 `image_cache`（LRU 上限
4 张，防 RGBA 内存无界增长）；渲染器零解码。历史上有三个被修掉的坑均与
"在渲染路径上做 IO"有关：`▶` 指针读文件造成与光标同节奏闪烁（现收敛到
`refresh_cur` 的 500ms TTL 缓存 + 只在消息路径刷新）、UI 线程同步解码造成
帧冻结、`Handle::from_path` 按 `.bin` 猜格式导致 tiny-skia 线程 panic。

**搜索**：≥3 字符走 core 的 FTS（后台线程 + `(query, gen)` 双新鲜度缓存，
过期丢弃不闪烁），再以 fzf 风格评分重排；<3 字符回落内存子序列过滤。
`/` 或 Ctrl-F 进入搜索态，Esc 退出并轮换输入框 Id（丢弃焦点使光标熄灭 →
窗口回到零重绘待机，这是最后一次闪烁修复）。

**与 fzf 版的对齐差异（任务 2.6 待收敛，已立项）**：

> 下表是 2.6 收敛前后的对照；**差异已全部消除**（星标符号除外，属有意的字形选择）。

| 维度 | 收敛前 fzf TUI | 收敛前原生 UI | 收敛后（2.6 已落地） |
|---|---|---|---|
| ★ 删除确认 | 15s TTL + `state/pending_delete` 落盘 | 内存布尔，**无 TTL** | `core::confirm` 单一状态机，15s TTL（ADR-005） |
| 载入窗口 | `min(max_items, 300)` | 全量 `max_items`（默认 750） | `store::MENU_LIMIT`=300，两后端一致 |
| 多行预览 | `tui preview_id`（100 行 / 300 字符） | `view.rs` 自建（80 行 / 300 字符） | `preview::preview_multiline()` 唯一实现 |
| 退出路径 | 进程自然结束 | `std::process::exit(0)` ×2 | 改 `iced::exit()`，走正常析构（D3） |
| 实例互斥 | — | PID 文件 + `/proc` 复核 | `core::single_instance` flock（C4） |
| 搜索范围 | fzf 内嵌过滤（300 行窗口） | 全量 + FTS/LIKE 全库 | 两后端都由 `store::search` 覆盖全库 |
| 星标符号 | `★` | `◆`（字体字形覆盖考虑） | **有意保留差异**，README 已写明 |
