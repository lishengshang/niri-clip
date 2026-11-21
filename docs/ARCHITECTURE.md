# niri-clip 架构

剪贴板采集、历史存储与选择器在独立进程中运行，共享 SQLite 数据库。

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
        │ idx_hash, idx_pinned_ts, idx_ts│
        └────────┬──────────────────────┘
                 │ list(min(max_items, MENU_LIMIT)) 直查（无缓存层）
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

**分层边界**：`niri-clip-core` 是**纯逻辑库**（`backup` / `confirm` / `config` /
`daemon` / `migrate` / `notify` / `preview` / `single_instance` / `store`），**不含 UI 后端选择
与终端编排**——fzf/fuzzel 的进程编排、终端探测、fzf 版本门控住在
`crates/niri-clip/src/tui.rs`（CLI 职责），原生 UI 不为这些代码买单。
（任务 2.6 / D1 已把 `tui` 从 core 迁出，此前的分层债已还清。）

## 2. 剪贴板采集

daemon 持有单实例锁，使用单线程 Tokio 运行时等待 `wl-paste`。打开 PRIMARY 选区时另起一个监听进程。空闲时两个选区都等待 Wayland 事件。一次性 CLI 命令直接运行。[Tokio 运行时](https://docs.rs/tokio/1.53.1/tokio/attr.main.html)

```text
wl-paste --watch timeout <秒数>s <niri-clip 路径> store
wl-paste --primary --watch timeout <秒数>s <niri-clip 路径> store
```

`--watch` 后面是命令及其参数。`wl-paste` 会根据 stdout 文件名推断 MIME，因此监听进程将 stdout 指向 `/dev/null`，诊断写入 stderr。超时值取自 `capture_timeout_secs`。[wl-paste 参数与 MIME 选择](https://github.com/bugaevc/wl-clipboard/blob/v2.3.0/src/wl-paste.c)

`store` 从 stdin 读取载荷。启用图片时通过文件头识别 PNG、JPEG、WebP，沿同一输入流按图片限额读取。文本按文本限额和过滤规则处理。未知格式可能触发系统剪贴板回读。超限、过滤和去重均在入库路径处理。

缺少 `wl-paste` 时，daemon 退回到每 500 ms 读取一次普通选区。该路径可能漏掉两次读取间的复制，开启图片时还会探测图片 MIME。正常部署使用 `wl-clipboard`。服务配置见[安装说明](../README.md)。

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
CREATE INDEX idx_ts ON clips(ts DESC, id DESC);   -- v5（任务 2.7）：list() 非 pinned_on_top 分支的 ts 序扫描
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
  见 ADR-003）；4→5 补 idx_ts（2.7，纯增索引无损升级）。此后 schema 变更必须
  新增版本号与迁移步骤
- **插入原子性**：SELECT 去重检查 + INSERT 包在 `BEGIN IMMEDIATE` 事务里。
  否则多进程并发（典型：fzf 选中旧条目 → wl-copy 写回 → daemon 同时捕获）
  双双通过检查后一方撞 UNIQUE 报错被静默吞掉
- **排序时间**：捕获和取消固定都会刷新 `ts`。取消固定使用 `idx_pinned_ts`
  取得普通历史最新时间，在同一条 SQL 中翻转标记并写入更晚的时间。日期清理也按 `ts` 计算。
- **上限裁剪**：超过正数容量上限时，`enforce_max_items` 保护固定条目与当前项，
  其余条目按 `ts` 从旧到新删除。文本和图片共用该入口。
- **菜单直查**：`list(min(max_items, MENU_LIMIT))`，MENU_LIMIT=300（2.6/D1 由
  `TUI_LIMIT` 更名：`tui` 移出 core 后该常量语义已是"菜单取数窗口"，与 UI 后端无关）。
  进程内缓存层已移除——fzf 每次 reload-sync spawn 全新 `list-raw` 进程，
  每个进程拥有独立地址空间，列表查询直接使用 SQLite 索引
- **全库搜索（2.1）**：`store::search` ≥3 字符走 `clips_fts MATCH` 短语查询
  + bm25 相关度（FTS5 的 MATCH 左侧必须是 fts 表名本身，别名会被当列名）；
  <3 字符退化为 LIKE 线性扫描（trigram 对短查询无增益，通配符已转义）。
  GUI 搜索经后台线程取候选（(query, gen) 双新鲜度缓存，过期丢弃），再以
  fzf 风格评分重排保持 UX 一致；CLI 暴露 `search` 子命令；fzf TUI 内嵌
  过滤保持 fzf 自身模糊匹配。trigram 按字面索引标点：跨标点短语不命中
  （子串语义，非词边界），单测锁定
- **基准**采用 Criterion，在独立 XDG 目录中用公开入库接口生成数据。基准在计时前断言行数和命中数。复现命令与实测结果见 §9。

- **大库长稳设施（2.5）**：`crates/niri-clip-core/tests/large_db.rs`——100k 条
  规模下跑写入 / 查询 / 并发 / 维护 / 迁移五段，每段自带条目数与内容断言。
  迁移段在**自建的 v3 旧库**上运行（照上一小节的 v1/v2/v3 步骤构造，含
  "同文本不同旧 hash"的重复行与星标继承），因此覆盖了外壳脚本无法触及的
  路径——这也是它必须留在 core 集成测试里的原因。**默认 `#[ignore]`**：耗时
  以分钟计，不进 PR 门禁；规模由 `NIRI_CLIP_STRESS_N` 覆盖（便于二分定位），
  沙盒位置由 `NIRI_CLIP_STRESS_DIR` 指定（`/tmp` 为小容量 tmpfs 的环境下必需，
  否则会以 `SQLITE_FULL` 死在写入中途）。实测数值见 §9

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
  绑定 `ctrl-p:pin{4}` / `ctrl-x:delete --fzf {4}` / `ctrl-d:delete-current`
  / `ctrl-y:copy {4}` / `ctrl-r:list-raw` / `alt-1..9:pos(n)+accept`，各动作后接 `reload-sync`
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
`crates/niri-clip-core/src/config.rs` 的 `defaults` 模块（任务 2.6/D5 已收敛为
单一来源，`Default::default()` 与 serde 缺省由一致性单测锁定），注释版样例见
`config/config.toml.example`。

读取时机：每个子命令入口 / 入库时各读一次（daemon 轮询循环内亦每 tick 读取，
成本为一次小文件 IO）；基于 mtime 监听的真正热重载列入选型 backlog，
当前语义是"改动即刻生效于下一次调用"。

`tui_backend` 四态：`auto`（native→fzf→fuzzel）/ `native` / `fzf` / `fuzzel`。
后端选择逻辑见 §4 与 §10。

## 6. 打包

- `PKGBUILD` `cargo build --release --locked` → `/usr/bin/niri-clip` + `/usr/share/doc` + `systemd user`
- `PKGBUILD.git` `git+https://...` + `pkgver()`
- `AUR` `makepkg --printsrcinfo > .SRCINFO`
- systemd 单元内置 22 项沙箱指令（AF_UNIX + 自身 state 最小面）；指令取舍、
  实测评分与强化档见 `SECURITY-AUDIT.md` §③（该文是沙箱细节的唯一出处）

## 7. 与 cliphist 关系

**完全独立**：`v1.0` 不再双写，`cliphist` 仅作为 `migrate` 源。旧版 `Mod+Shift+V` 可保留为 `cliphist` 独立入口，但默认 `niri-clip` 不感知。

## 8. 为什么 Rust + SQLite + fzf

- **Rust**：`<40MB` 常驻，`tokio` 异步，无 `fork`，发 `AUR` 最稳
- **SQLite WAL**：单文件备份，`FTS5` 搜索，`WAL` 读写不锁
- **fzf**：`--track` 是唯一“删除不跳顶”不闪的实现，`ratatui` 自绘后续可选

<a id="performance"></a>

## 9. 性能与开销

### 查询与进程开销

2026-11-21，Arch Linux x86_64，16 个可用逻辑核，Rust 1.100.0，release 构建。以下数据来自合成历史，SQLite 位于 tmpfs。

| 一万条记录，取 300 条 | 每次耗时 | 计时范围 |
|---|---:|---|
| `list_300_of_10k` | 0.992 ms | 配置、连接、当前项、排序与完整条目 |
| `sqlite_select_300_of_10k` | 0.068 ms | 复用连接，按时间读取 300 行，Rust 端取 id |
| `fts_search_300_of_10k` | 11.985 ms | 新连接，三字词「中英文」命中全库，按相关度和时间返回前 300 条 |

```sh
cargo bench --locked -p niri-clip-core --bench store -- --noplot --output-format bencher
```

旧基准在 `b01cad0` 仅保留 750 条，搜索词「基准中」没有命中。现按种子数设置容量，计时前断言库存和返回行数。两组数据无法用于计算提速比。

| 相同 CLI 工作负载 | `b01cad0` | 本轮 |
|---|---:|---:|
| `store` 等待 stdin 时的线程数 | 17 | 1 |
| 重复捕获同一条短文本 | 4.306 ms | 3.098 ms |
| `list-raw` 输出单条历史 | 4.782 ms | 3.991 ms |

每次启动独立进程，表中时间为预热后一百次的中位数，包含操作系统启动进程的开销。对照版本可在各自的源码目录运行以下命令。

```sh
cargo build --release --locked -p niri-clip
python3 - <<'PY'
import os, pathlib, statistics, subprocess, tempfile, time
with tempfile.TemporaryDirectory() as d:
    env = dict(os.environ, XDG_STATE_HOME=d+'/state', XDG_CONFIG_HOME=d+'/config', XDG_CACHE_HOME=d+'/cache')
    exe = 'target/release/niri-clip'
    with subprocess.Popen([exe, 'store'], env=env, stdin=subprocess.PIPE, stderr=subprocess.DEVNULL) as p:
        time.sleep(0.2)
        print(next(x for x in pathlib.Path(f'/proc/{p.pid}/status').read_text().splitlines() if x.startswith('Threads:')))
        p.communicate(b'sample')
    for cmd in ('store', 'list-raw'):
        ms = []
        for _ in range(101):
            start = time.perf_counter_ns()
            subprocess.run([exe, cmd], env=env, input=b'sample', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            ms.append((time.perf_counter_ns() - start) / 1e6)
        print(cmd, statistics.median(ms[1:]), 'ms')
PY
```

在嵌套 niri 26.04、wl-clipboard 2.3.0 中，空闲 daemon 的 RSS 约 8 MiB，每个 `wl-paste` 约 2.3 MiB。启用 PRIMARY 后共三个常驻进程，各一个线程，连续五秒内 CPU 计时均无增量。RSS 会重复计算共享页。可在运行 daemon 的测试会话中查看：

```sh
pid=$(pgrep -nx -f '.*niri-clip daemon')
ps -o pid,ppid,rss,nlwp,comm -p "$pid" --ppid "$pid"
for pass in 1 2; do
  for p in "$pid" $(pgrep -P "$pid"); do awk '{print $1, $14+$15}' "/proc/$p/stat"; done
  [ "$pass" = 2 ] || sleep 5
done
```

### 原生窗口启动

嵌套 niri 中，缓存预热后各启动十次。从执行 `niri-clip tui` 到 niri IPC 首次列出窗口，空库中位数为 82.7 ms，有 300 个可见候选时为 108.0 ms。该测量包含 CLI、GUI 初始化和 IPC 采样。

日常 niri 会话已复测。空库与满菜单均超过路线图的可交互目标。测量使用独立 XDG 测试库和 release 构建，由 Linux `uinput` 触发实际 `Mod+V` 绑定。从发送 `V` 按下前的单调时间计起，等 niri 聚焦窗口及 GUI 报出 `Refocus`，发送 `/` 进入搜索，再以 `NIRI_CLIP_DEBUG=1` 输出首个 `Query` 为终点。每种场景分别报告该批首次打开和重复打开的中位数，使用系统当时的页缓存。

在独立 niri 会话中，分别使用空库和有 300 条以上合成历史的 XDG 目录。确保 `target/release` 在 PATH 首位，并关闭已打开的选择器。以下命令报告热启动中位数。

```sh
PATH="$PWD/target/release:$PATH" python3 - <<'PY'
import json, statistics, subprocess, time
def windows():
    items = json.loads(subprocess.check_output(['niri','msg','-j','windows']))
    return [w['id'] for w in items if w['app_id'] == 'niri-clip-gui']
samples = []
for _ in range(11):
    start = time.monotonic()
    subprocess.run(['niri-clip','tui'], check=True)
    while not (shown := windows()):
        if time.monotonic() - start > 5: raise TimeoutError('窗口未出现')
        time.sleep(.002)
    samples.append(1000 * (time.monotonic() - start))
    time.sleep(.16)
    subprocess.run(['niri','msg','action','close-window','--id',str(shown[0])], check=True)
    while windows(): time.sleep(.05)
print(statistics.median(samples[1:]), 'ms')
PY
```

### 图片预览内存

在同一嵌套会话中，以文本为初始选中项，逐张浏览四张 3840 × 2160 的 RGB PNG。原尺寸缓存下窗口 RSS 从 20.9 升至 203.3 MiB；缩略图缓存下从 21.4 升至 53.2 MiB。数值为各次解码结束后的进程 RSS。四个缩略图的 RGBA 数据合计最多 6.875 MiB（`4 × 1024 × 440 × 4 / 1024²`）。源图解码、渲染器与分配器占用另计。

在独立 Wayland 测试会话运行以下命令。需要 ImageMagick 与 wtype。两次 `ps` 分别取初始与浏览后的 RSS，单位为 KiB。

```bash
cargo build --release --locked -p niri-clip -p niri-clip-gui
(
  d=$(mktemp -d)
  export XDG_CONFIG_HOME="$d/config" XDG_STATE_HOME="$d/state" XDG_CACHE_HOME="$d/cache"
  mkdir -p "$XDG_CONFIG_HOME/niri-clip"
  printf '%s\n' 'enable_image_preview = true' 'notify_enabled = false' > "$XDG_CONFIG_HOME/niri-clip/config.toml"
  for rgb in 3264a0 5564a0 7864a0 9b64a0; do
    magick -size 3840x2160 "xc:#$rgb" PNG24:- | target/release/niri-clip store
  done
  printf sample | target/release/niri-clip store
  target/release/niri-clip-gui & gui=$!
  sleep 1
  ps -o rss= -p "$gui"
  wtype -s 600 -k Down -s 1000 -k Down -s 1000 -k Down -s 1000 -k Down -s 1000
  ps -o rss= -p "$gui"
  kill "$gui"
)
```

### 构建环境

使用 Rust 稳定版和仓库的 Cargo.lock。在 Linux x86_64 的锁定依赖中，CLI 经由 `home`、GUI 经由 `iced` 和 `image` 至少需要 Rust 1.88。本轮构建使用 1.100.0，最低可编译版本尚待独立验证。可用以下命令核对依赖声明。

```sh
rustc -V
cargo metadata --locked --format-version 1 | jq -r '.packages[] | select(.name == "home" or .name == "iced" or .name == "image") | [.name, .version, .rust_version] | @tsv'
```

### 依赖与编译

2026-11-21，使用当前 Cargo.lock，在 Linux x86_64 上统计普通依赖。按包名和版本去重，包含根包，排除开发依赖。GUI 单独构建，安装 CLI 无须引入 iced。

| 范围 | 包数 | release 可执行文件大小 |
|---|---:|---:|
| niri-clip | 100 | 5,781,112 B |
| niri-clip-core | 83 | — |
| niri-clip-gui | 204 | 10,510,304 B |
| workspace | 222 | — |

```sh
for pkg in niri-clip niri-clip-core niri-clip-gui; do
  cargo tree --locked -e normal -p "$pkg" --prefix none | sed 's/ (\*)$//' | sort -u | wc -l
done
cargo tree --locked -e normal --workspace --prefix none | sed 's/ (\*)$//' | sort -u | wc -l
cargo build --locked --release -p niri-clip
cargo build --locked --release -p niri-clip-gui
stat -c '%n %s' target/release/niri-clip target/release/niri-clip-gui
```

- CLI 通过 wl-clipboard 监听事件，缺少 `wl-paste` 时回退到 wl-clipboard-rs。通知调用系统的 `notify-send`，由 coreutils `timeout` 限时，后台线程回收子进程。
- GUI 使用 iced 的 `tiny-skia`、`wayland` 和 `image-without-codecs`。图片读取、缩放和解码使用现有 image 库，只启用 PNG、JPEG、WebP。渲染器直接接收 RGBA 缩略图。
- release 启用 LTO、单代码生成单元和符号剥离，SQLite 随 rusqlite 编译。历史测量（2026-09-01）为 CLI 冷构建 77 s，随后编译 GUI 另需 108 s。本轮复测了包数与可执行文件大小，冷构建耗时沿用该次记录。

用独立目标目录复测冷构建，可保留日常开发缓存。

```sh
(
  export CARGO_TARGET_DIR="$(mktemp -d)"
  time cargo build --release --locked -p niri-clip
  time cargo build --release --locked -p niri-clip-gui
)
```

### 大库长稳复测

2026-11-21，Linux x86_64、Rust 1.100.0，release 构建，SQLite 数据及临时文件位于 tmpfs。测试先插入十万条种子，再执行逐条捕获、查询、并发写入、内存稳态、维护与旧库迁移。写入耗时包含文件系统开销。

```sh
NIRI_CLIP_STRESS_DIR="$(mktemp -d)" cargo test --locked --release -p niri-clip-core \
  --test large_db -- --ignored --nocapture
```

| 操作 | 实测 |
|---|---|
| 单事务写入十万条 | 4.1 s，库文件 59 MiB |
| 逐条捕获 / 列表读取 300 条 | 0.86 ms/条 / 1.13 ms |
| FTS 搜索 / 短词 LIKE | 5.72 ms / 0.43 ms，取决于词频和结果分布 |
| 并发写入 | 4 个线程各写入 250 条，零丢失 |
| 重复 200 轮列表和搜索 | RSS 约 12.1 MiB，报告的增量约 0.1 MiB |
| 清理与压缩 | 预估与实际删除数一致，59.3 → 25.7 MiB |
| v3 迁移 | 合并 500 组重复，耗时 1.9 s |

`list()` 先按 `idx_hash` 点查当前项，再按固定状态和时间顺序读索引。v5 为按时间排列的分支增加了 `idx_ts`。短词 LIKE 的未命中查询可能遍历整库，表中的时间只适用于测试数据。

## 10. 原生窗口

原生选择器按需启动，窗口关闭时退出。它使用 iced、winit 的常规 xdg 窗口及 tiny-skia 软件渲染。niri 根据 app-id `niri-clip-gui` 应用浮动、位置和阴影规则。winit 负责输入法事件。[选型 ADR-001](ADR-001.md) · [窗口规则示例](../assets/niri-clip.kdl)

**后端选择（`tui_backend`）**：`auto` 优先 native → fzf → fuzzel；
`gui_binary()` 三级兜底定位二进制（`PATH` → 与 CLI 同目录 → `~/.cargo/bin`），
因 niri `spawn` 环境的 PATH 常缺 `~/.cargo/bin`（真机踩坑，否则 auto 会误降级）。
显式 `"native"` 但二进制缺失时降级 fzf/fuzzel。fzf 版本地板 0.71
（`--id-nth`）在 `run()` 内二次校验。

**单实例与聚焦**：经 `core::single_instance::InstanceGuard::try_acquire("gui")`
对 `state/gui.lock` 加 flock（与 daemon 同一机制，2.6/C4 已收敛）；锁被占用
则经 niri IPC（`niri msg -j windows`，按 app_id `niri-clip-gui` 精确匹配）
聚焦已开窗口后本进程退出。进程崩溃由内核释放锁，无 PID 文件与 `/proc` 复核。

**渲染与预览**。列表使用单行预览，底部窗格显示多行文本或图片。iced 的线程池负责搜索、复制和图片解码。每次解码一张，完成后检查当前选中项。缩略图保留原比例，宽度上限为 1024 像素，高度上限为 440 像素，窗口缓存最近解码的四张。复制直接读取原图文件。单张源图解码仍需要与原始分辨率相应的临时内存。[image 缩略图接口](https://docs.rs/image/0.25.10/image/enum.DynamicImage.html#method.thumbnail)

**搜索**。点击输入框或按 `/`、`Ctrl+F` 开始搜索。输入时先显示当前窗口的子序列匹配，同时后台查询 SQLite。三字符起走 FTS5 trigram，短词走 LIKE。同一时间只运行一个数据库查询，输入变化后接续最新请求。结果返回时核对输入和历史列表版本，仅使用仍匹配的结果，并重新定位选中条目。空历史、查询中和无匹配各有对应提示。[GNOME 搜索交互规范](https://developer.gnome.org/hig/patterns/nav/search.html)

| 行为 | fzf | 原生窗口 |
|---|---|---|
| 初始列表 | 最近窗口，最多 300 条 | 最近窗口，最多 300 条 |
| 搜索范围 | 载入的条目 | 全库候选，最多显示 300 条 |
| 固定条目 | `★` | `◆` |
| 删除确认 | 列表行尾提示 | 窗口横幅 |

两个前端共用 `core::confirm` 的删除确认和 `preview::preview_multiline` 的文本截断规则。[交互决策 ADR-005](ADR-005.md)
