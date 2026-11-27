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
| `current` | ▶ 最近捕获或从历史复制的条目指纹。捕获跳过和空选区事件清除该标记 | 可（退化为无 ▶ 标记） |
| `current.lock` | 短时串行化当前项更新和清除 | 进程停止后可删除 |
| `migration.lock` | 串行化旧库搬迁、schema 迁移和快照发布 | 进程停止后可删除 |
| `daemon.lock` | daemon 单实例 flock（`core::single_instance`） | 可（内核释放，无陈锁） |
| `gui.lock` | 原生 UI 单实例 flock（同一机制，2.6/C4 起不再用 PID 文件） | 可（内核释放） |
| `pending_delete` | ★ 条目二段确认挂起态（id + 时间戳，15s TTL）——语义与判定归 `core::confirm`（ADR-005）；fzf TUI 用行尾标记呈现、原生 UI 用横幅、CLI 用提示文案 | 可（TTL 到期或显式 `clear()` 即清） |
| `fzf.version` | fzf 版本门控缓存（按可执行文件的设备、inode 和 ctime 识别） | 可（下次打开时重新探测） |
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

`--watch` 后面是命令及其参数。`wl-paste` 会根据 stdout 文件名推断 MIME，因此监听进程将 stdout 指向 `/dev/null`，诊断写入 stderr。超时值取自 `capture_timeout_secs`，为 0 时关闭超时。任一监听进程意外退出时，daemon 回收另一条监听并返回失败，使 systemd 的 `Restart=on-failure` 生效。[wl-paste 参数与 MIME 选择](https://github.com/bugaevc/wl-clipboard/blob/v2.3.0/src/wl-paste.c)

`store` 从 stdin 读取载荷。启用图片时先读取识别 PNG、JPEG、WebP 所需的头部，再按对应限额续读同一输入流。文本按文本限额和过滤规则处理，规则无法编译时暂停文本捕获。`CLIPBOARD_STATE` 标为敏感或空时跳过读取。未知格式可能触发剪贴板回读。普通剪贴板捕获到重复条目仍刷新当前标记，观察到变更却未保留内容时清除原有标记。清除前在短锁内核对修订，保留并发复制产生的新标记。PRIMARY 捕获仅刷新历史。

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
  见 ADR-003）；4→5 补 `idx_ts`；5→6 从 `clips` 在事务内重建 FTS，以修复旧版并发回填产生的重复索引记录。旧库搬迁与升级共用 `migration.lock`，快照写完再发布。已升级的库正常打开时跳过迁移锁。
- **插入原子性**：SELECT 去重检查 + INSERT 包在 `BEGIN IMMEDIATE` 事务里。
  否则多进程并发（典型：fzf 选中旧条目 → wl-copy 写回 → daemon 同时捕获）
  双双通过检查后一方撞 UNIQUE 报错被静默吞掉
- **图片生命周期**：入库和导入共用图片文件写入路径，在写事务中发布文件。孤儿清理也持有 SQLite 写锁，确保待提交图片不会被当作孤儿。淘汰先提交行删除，再清理关联文件。
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

- **导出/回灌（2.4，backup.rs）**：NDJSON 流式格式内嵌图片，读事务保证行数和扫描一致。NDJSON 与 `VACUUM INTO` 快照均以 0600 临时文件写完后发布，已有目标会报错。回灌按 hash 去重并校验内容，保留时间、固定状态和当前标记。预检用只读连接统计，也能读取尚未升级的历史。 [格式与文件边界](ADR-004.md)

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
- core crate 经本地符号链接复用系统服务模板和配置样例。Cargo 打包时把两者作为普通文件收进独立归档，可在工作区之外构建和运行测试。
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

日常 niri 会话中，使用独立 XDG 测试库和 release 构建，由 Linux `uinput` 触发实际 `Mod+V` 绑定。从发送 `V` 按下前的单调时间计起，等 niri 聚焦被测 GUI 及该进程报出 `Refocus`，发送 `/` 进入搜索，再键入 `q`。计时止于测量器从 `NIRI_CLIP_DEBUG=1` 日志读到首个 `Query`，包括发键和观察日志的耗时。

分段复测在临时源码的 CLI `main` 入口、启动 GUI 前、GUI `main` 入口和首批 `store::list` 前后记录 `CLOCK_MONOTONIC`。用 `WAYLAND_DEBUG=client` 找到主窗口首次挂载非空缓冲区后的 `wl_surface.commit`，将协议日志的 UTC 时间换算到同一单调时钟。搜索阶段沿用 `Refocus` 到首个 `Query` 的输入流程。两种构建交替运行，普通 release 用于对照总延迟。开始发键前等待 niri 发出新的 `ConfigLoaded` 事件，确认临时绑定已生效。

满菜单的最长单段为读完历史到提交首帧。空库的最长单段为从按下快捷键到进入 CLI，整段启动链路与 GUI 首帧前的开销接近。两种场景下 CLI 入口到派生 GUI、首批历史读取都较短。满菜单优先检查窗口布局和首帧渲染，空库另查 niri 触发绑定到 CLI 入口之间的耗时。

进一步在临时构建中使用 iced 的布局计时点，并单独包住 tiny-skia 的 `renderer.draw`。满菜单提交首帧前，布局是耗时最大的阶段，布局内包含首次加载系统字体和列表文字排版。空库以同一原生启动器对照快捷键、niri IPC `Spawn` 和直接启动。快捷键与 IPC 到启动器的耗时接近，主要开销位于 niri 的共同启动链路，启动器进入 CLI 较快。niri 的 `Spawn` 使用子线程和双重 `fork`。[niri 26.04 启动实现](https://github.com/YaLTeR/niri/blob/v26.04/src/utils/spawning.rs)

在同一 niri 会话中交替运行改前和按视口布局的 release，保持合成历史、启动器和发键口径一致。满菜单的首帧布局、端到端延迟和首字符时的 RSS 均下降。独立 Wayland 会话覆盖滚到末行、从深部视口搜索至单一结果，并逐字比对两次复制回读。

在 `iced_tiny_skia::Renderer::draw` 同时记录 `CLOCK_MONOTONIC` 和 `CLOCK_THREAD_CPUTIME_ID`，统计首帧的物理像素尺寸、绘制图元和文字，并用 `sched_getcpu` 采样渲染线程所在核心。通过 `lscpu -e=CPU,CORE,MAXMHZ` 区分本机两类核心，再分组设置 CPU 亲和性，相当于给临时构建加 `taskset -c 4-7` 或 `taskset -c 8-11` 前缀。两版首帧绘制规模相同，限制在同一类核心时耗时接近。自由调度时两版落到不同核心的比例不同，解释了先前软件绘制的大部分时差。实际快捷键复测使用未限制 CPU 的普通 release。

空库交替对照的 GUI 入口至首轮软件绘制完成耗时未显示稳定退化。下表用同轮配对比较整段启动。

原生窗口的普通历史行共用窗口底色，选中行使用实色圆角高亮，减少 tiny-skia 每帧重复填色和阴影合成。字体库由短生命周期线程提前触发 iced 的共享初始化，与 Wayland 连接、首批历史读取重叠。首次排版仍使用同一份字体库，保留系统字体回退。

#### 真机快捷键到首个搜索字符（2026-11-21）

在同一 niri 26.04 会话中，临时将 `Mod+V` 绑定到原生测量启动器。三个版本共用同一 CLI，经 `niri-clip tui` 启动对应 GUI。空库已建好表，满库文本为 `query sample NNN | niri clipboard 实机启动测试样本`（`000` 到 `299`）。每次重新启动 GUI，沿用系统页缓存和默认 CPU 调度。每库每版单列本批首次打开，再测 48 轮，每轮各版各开一次，六种顺序各用八轮。P90 取最近秩，RSS 取处理首字符时 GUI 进程 `VmRSS` 的中位数。

| 历史 | GUI 版本 | 首次 (ms) | 重复中位数 (ms) | P90 (ms) | 重复中 ≤50 ms | GUI RSS (MiB) |
|---:|---|---:|---:|---:|---:|---:|
| 0 | 优化前 `fe84a4e` | 59.8 | 64.5 | 74.5 | 2/48 | 20.9 |
| 0 | 按视口布局 `ad4494c` | 70.1 | 64.7 | 76.9 | 1/48 | 20.9 |
| 0 | 再加字体预热与行绘制优化 | 65.3 | **59.3** | 66.2 | 1/48 | 21.3 |
| 300 | 优化前 `fe84a4e` | 69.9 | 85.3 | 102.0 | 0/48 | 29.3 |
| 300 | 按视口布局 `ad4494c` | 73.5 | 77.3 | 92.1 | 0/48 | 22.4 |
| 300 | 再加字体预热与行绘制优化 | 71.8 | **66.9** | 77.4 | 0/48 | 22.6 |

两种库的重复中位数均超出 50 ms。空库中，按视口布局相对优化前的同轮差值中位数为 −0.9 ms，配对 bootstrap 95% 区间为 [−3.3, +3.6] ms。上批观察到的变慢与波动相容，本批仍容许小幅退化。再加字体和行绘制优化后，相对视口版的配对差值在空库为 −6.0 ms [−9.1, −1.4]，在 300 条库为 −9.5 ms [−15.5, −3.9]。该批首帧优化版从按键到启动器入口的中位数分别为 25.5 和 24.9 ms，从启动器到首字符分别为 33.7 和 42.0 ms。

#### 位置记忆和 GUI 线程数的配对复测

两批测试都沿用上面的按键、焦点、首字符和独立数据库口径。每格单列批次内首次打开，余下 48 轮按顺序轮换版本。第一批比较已提交的首帧优化、位置记忆和一个只在命中搜索时排版彩色文字的候选。第二批以位置记忆版为对照，仅调整 GUI 的工作线程。两版均按需启动新进程并加载完整的系统字体。

| 批次与版本 | 空库首次 / 中位数 / P90（ms） | 300 条首次 / 中位数 / P90（ms） |
|---|---:|---:|
| 第一批 · 首帧优化 `532995e` | 73.3 / 65.8 / 74.6 | 62.7 / 74.6 / 83.6 |
| 第一批 · 位置记忆 `acfd56f` | 77.3 / 65.2 / 69.9 | 67.5 / 72.6 / 82.4 |
| 第一批 · 单色历史行候选 | 74.8 / 65.9 / 71.7 | 72.7 / 74.9 / 85.7 |
| 第二批 · 位置记忆 `acfd56f` | 77.2 / 64.5 / 73.2 | 67.6 / 72.8 / 82.6 |
| 第二批 · 按需创建阻塞线程 | 66.9 / 65.6 / 72.1 | 77.2 / 72.1 / 83.8 |

两批中每格的 48 次重复都超过了 50 ms。空库没有复现稳定变慢。单色历史行候选相对位置记忆版的配对差值的 95% 区间在两种库中均跨过零，已撤下。[第一批逐次样本](benchmarks/gui-startup-position-2026-11-21.csv)

第二批在首字符时同时读取 GUI 进程的 `Threads` 和 `VmRSS`。线程数中位数在两种库中都由 18 降至 4。空库两版 RSS 分别为 21.38 和 21.42 MiB，300 条分别为 22.65 和 22.67 MiB。按需线程版相对对照的同轮延迟差值中位数为空库 +0.4 ms，300 条 +2.7 ms，两组的配对 bootstrap 95% 区间分别为 [−2.3, +3.4] 和 [−3.5, +6.5] ms。按键至启动器入口的中位数是 32.8 和 31.8 ms，启动器至首字符是 31.6 和 40.9 ms。两个分段各自取中位数。[第二批逐次样本](benchmarks/gui-startup-workers-2026-11-21.csv)

从仓库根目录可用以下命令分别复算三个批次。CSV 按采集顺序保存，`block=0` 是各库各版在该批的首次打开，时钟均相对同次 `V` 按下。最早批次中的 `tuned` 对应首帧优化版。[首帧优化批次样本](benchmarks/gui-startup-2026-11-21.csv)

```sh
python3 - <<'PY'
import csv, math, random
from collections import defaultdict
from itertools import combinations
from pathlib import Path
from statistics import median

for path in sorted(Path('docs/benchmarks').glob('gui-startup-*.csv')):
    with path.open() as f:
        rows = list(csv.DictReader(f))
    groups = defaultdict(dict)
    for row in rows:
        groups[int(row['history_rows']), row['build']][int(row['block'])] = row
    builds = list(dict.fromkeys(row['build'] for row in rows))
    print(path.name)
    for size in (0, 300):
        for build in builds:
            group = groups[size, build]
            repeats = [group[k] for k in range(1, 49)]
            q = sorted(float(r['query_ms']) for r in repeats)
            launch = [float(r['launcher_ms']) for r in repeats]
            after = [float(r['query_ms']) - t for r, t in zip(repeats, launch)]
            rss = median(int(r['gui_rss_kib']) / 1024 for r in repeats)
            threads = [int(r['gui_threads']) for r in repeats if r.get('gui_threads')]
            print(size, build, group[0]['query_ms'], median(q), q[math.ceil(.9*len(q))-1],
                  sum(t <= 50 for t in q), len(q), rss, median(launch), median(after),
                  median(threads) if threads else '')
        for old, new in combinations(builds, 2):
            a, b = groups[size, old], groups[size, new]
            delta = [float(b[k]['query_ms']) - float(a[k]['query_ms']) for k in range(1, 49)]
            rng = random.Random(43)
            boot = sorted(median(rng.choices(delta, k=len(delta))) for _ in range(8000))
            print(size, new, 'minus', old, median(delta), boot[200], boot[7799])
PY
```

<a id="gui-standby"></a>

#### 可选短时待机的冷热验收

真机使用同一 release，将独立配置的 `gui_idle_timeout_secs` 设为 30。每库测 49 对，先开新 GUI 进程，再在待机期间重开。每对核对 PID 相同、窗口 ID 不同，每次发搜索键前核对焦点。进程冷启动允许系统页缓存已预热。为在同一时段交替取得两种样本，前 48 对完成后结束该测试库已无窗口的 GUI。最后一对让它自然退出后再按一次快捷键。三种情况均从发送 `Mod+V` 中的 `V` 按下算到 GUI 处理首个搜索字符。[198 次逐次样本](benchmarks/gui-standby-2026-11-21.csv)

| 历史 | 打开方式 | 本批首个样本 (ms) | 其余 48 次中位数 (ms) | P90 (ms) | ≤50 ms |
|---:|---|---:|---:|---:|---:|
| 0 | 新进程 | 69.10 | 68.16 | 75.60 | 0/48 |
| 0 | 待机内重开 | 46.23 | 50.65 | 62.62 | 18/48 |
| 300 | 新进程 | 64.71 | 72.43 | 82.52 | 0/48 |
| 300 | 待机内重开 | 55.77 | 53.46 | 67.15 | 15/48 |

同轮逐对计算，空库待机重开快 16.82 ms，bootstrap 95% 区间为 12.63–20.23 ms。300 条库快 19.55 ms，区间为 14.64–22.49 ms。两组的待机重开中位数和 P90 仍超过 50 ms。待机重开时，按键至启动器入口的中位数分别为 33.44、30.36 ms，启动器入口至首字符为 16.55、23.49 ms。自然超时后打开各只有一次，空库 64.31 ms，300 条库 83.84 ms。

关闭窗口后约一秒读取无窗口 GUI 的 `VmRSS`，空库 17.46 MiB，300 条库 18.98 MiB，各有 3 个线程。随后五秒内，两者的进程用户态与内核态 CPU 时间合计均增加 0 个时钟滴答，本机每滴答 10 ms。确认窗口消失并等待约 0.13 秒后开始监测退出，两库分别再过 29.85、29.86 秒退出，唤醒端点随即消失。[待机原始读数](benchmarks/gui-standby-idle-2026-11-21.csv)

独立 Wayland 会话验证了待机期间写入的合成历史可在重开时选中并逐字节复制。上次查询已清空，原浮窗位置可恢复。多个启动请求同时到达时只出现一个窗口。

从仓库根目录可复算上表与分段耗时。每组的 `block=0` 单列，P90 取最近秩。

```sh
python3 - <<'PY'
import csv, math, random
from pathlib import Path
from statistics import median

root = Path('docs/benchmarks')
with (root / 'gui-standby-2026-11-21.csv').open() as f:
    rows = list(csv.DictReader(f))
for size in ('0', '300'):
    group = {(r['opening'], int(r['block'])): r for r in rows if r['history_rows'] == size}
    for kind in ('cold', 'warm'):
        sample = [group[kind, i] for i in range(1, 49)]
        times = sorted(float(r['query_ms']) for r in sample)
        launch = [float(r['launcher_ms']) for r in sample]
        after = [float(r['query_ms']) - t for r, t in zip(sample, launch)]
        print(size, kind, group[kind, 0]['query_ms'], median(times),
              times[math.ceil(.9 * len(times)) - 1], sum(t <= 50 for t in times),
              median(launch), median(after))
    gain = [float(group['cold', i]['query_ms']) - float(group['warm', i]['query_ms'])
            for i in range(1, 49)]
    rng = random.Random(43)
    boot = sorted(median(rng.choices(gain, k=len(gain))) for _ in range(8000))
    print(size, 'paired gain', median(gain), boot[200], boot[7799],
          'post-expiry', group['expired', 0]['query_ms'])
with (root / 'gui-standby-idle-2026-11-21.csv').open() as f:
    for r in csv.DictReader(f):
        print(r['history_rows'], int(r['gui_rss_kib']) / 1024, r['gui_threads'],
              r['cpu_ticks'], r['clock_hz'], r['sample_seconds'], r['exit_after_check_seconds'])
PY
```

<a id="selector-config"></a>

#### 选择器配置读取的配对复测

对照为短时待机版 `3b181af`。候选版用同一份 TOML 解析与校验逻辑提取菜单设置，首批列表直接使用本次读取的置顶顺序。捕获与 `doctor` 仍编译配置中的过滤规则。独立库的 `list-raw` 每轮各启动一次，两版的输出逐字节一致。单列首轮后，48 轮中位数在空库从 5.88 降至 2.96 ms，在 300 条库从 7.04 降至 3.75 ms。此段从调用 CLI 算到读完行输出。[逐次 CLI 样本](benchmarks/selector-config-list-2026-11-21.csv)

真机在本批开始时以当前日常配置为底本切换测试绑定。两版均设置 30 秒待机，共用同一启动器和两套合成数据库。在每轮中轮换库和版本的顺序，每版每库先打开新进程，再从待机重开，确认同一 PID 上有两个不同的窗口。随后结束该测试库的无窗口进程，为下一对准备。每次均按实际快捷键，核对焦点后进入搜索，计时截至首个 `Query`。每组首轮单列，下表统计其余 48 轮。RSS 在首字符时从 GUI 进程读取。[392 次快捷键样本](benchmarks/selector-config-hotkey-2026-11-21.csv)

| 历史 | 打开方式 | 对照中位数 / P90 (ms) | 候选中位数 / P90 (ms) | ≤50 ms 对照 → 候选 | RSS 中位数对照 → 候选 (MiB) |
|---:|---|---:|---:|---:|---:|
| 0 | 新进程 | 66.25 / 70.29 | 64.31 / 71.74 | 1/48 → 0/48 | 21.56 → 19.96 |
| 0 | 待机重开 | 47.78 / 59.71 | 47.26 / 55.77 | 29/48 → 31/48 | 22.17 → 20.16 |
| 300 | 新进程 | 71.09 / 79.12 | 69.56 / 80.25 | 0/48 → 0/48 | 22.92 → 21.46 |
| 300 | 待机重开 | 57.95 / 65.77 | 54.60 / 63.98 | 15/48 → 11/48 | 23.41 → 21.72 |

按同轮配对，候选减对照的端到端差值中位数及 bootstrap 95% 区间分别为空库冷启动 −2.58 [−4.45, +0.13] ms、空库待机 −4.00 [−5.66, +2.73] ms、300 条冷启动 −1.30 [−5.81, +1.25] ms、300 条待机 −1.20 [−4.92, +3.31] ms。四个区间均包含零。空库待机的两版中位数均低于 50 ms，候选版在 300 条库的达标次数反而少了四次。可确认的收益是 CLI 取数时间缩短、首字符时 GUI 占用下降，以及 GUI release 从 11,003,040 B 缩至 9,596,256 B。两个程序的构建命令见本节“依赖与编译”。

从仓库根目录可复算这些批次。数据库内容和编译设置相同，首轮在 CSV 内标记为 `block=0`。

```sh
python3 - <<'PY'
import csv, math, random
from pathlib import Path
from statistics import median

for path in sorted(Path('docs/benchmarks').glob('selector-config-*.csv')):
    with path.open() as f:
        rows = list(csv.DictReader(f))
    metric = 'query_ms' if 'opening' in rows[0] else 'elapsed_ms'
    for size, opening in sorted({(r['history_rows'], r.get('opening')) for r in rows}):
        by = {(r['build'], int(r['block'])): r for r in rows
              if (r['history_rows'], r.get('opening')) == (size, opening)}
        for build in ('standby', 'selector-config'):
            repeats = [by[build, i] for i in range(1, 49)]
            times = sorted(float(r[metric]) for r in repeats)
            rss = [int(r['gui_rss_kib']) / 1024 for r in repeats if 'gui_rss_kib' in r]
            print(size, opening, build, by[build, 0][metric], median(times),
                  times[math.ceil(.9 * len(times)) - 1], sum(t <= 50 for t in times),
                  median(rss) if rss else '')
        diff = [float(by['selector-config', i][metric]) - float(by['standby', i][metric])
                for i in range(1, 49)]
        rng = random.Random(43)
        boot = sorted(median(rng.choices(diff, k=len(diff))) for _ in range(8000))
        print('paired', median(diff), boot[200], boot[7799])
PY
```

#### niri 派生进程的单独测量

在同一日常会话中，交替由一个 Python 进程直接启动原生打点程序，或通过 niri IPC 的 `Spawn` 动作启动它。该程序在 `main` 入口读取单调时钟，只向本次测量的 FIFO 写入时间戳。IPC 计时从发送已编码的请求前开始，两种路径均使用程序内的时间戳作为终点。单列首次后，各重复 48 次。直接启动的中位数为 1.1 ms，P90 为 1.3 ms。经 niri 的中位数为 34.3 ms，P90 为 39.5 ms。这一批独立于快捷键采样，涵盖 IPC 处理、调度与创建进程。各环节的占比尚待定位。[逐次样本](benchmarks/niri-spawn-2026-11-21.csv)

在 niri 会话中从仓库根目录运行以下命令可重新采集，无需窗口或键盘输入。将读取路径改为 `docs/benchmarks/niri-spawn-2026-11-21.csv` 可复算上面的记录。

```sh
report=$(mktemp)
python3 docs/benchmarks/niri-spawn.py > "$report"
python3 - "$report" <<'PY'
import csv, sys
from statistics import median
with open(sys.argv[1]) as f:
    rows = list(csv.DictReader(f))
for route in ('direct', 'ipc'):
    times = sorted(float(r['entry_ms']) for r in rows if r['route'] == route and int(r['block']))
    print(route, len(times), median(times), times[43])
PY
```

#### 嵌套会话的窗口出现时间

嵌套 niri 中，缓存预热后各启动十次。从执行 `niri-clip tui` 到 niri IPC 首次列出窗口，空库中位数为 82.7 ms，有 300 个可见候选时为 108.0 ms。该测量包含 CLI、GUI 初始化和 IPC 采样。

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

2026-11-21，使用当前 Cargo.lock，在 Linux x86_64 上统计普通依赖。按包名和版本去重，包含根包，排除开发依赖。可执行文件大小取本轮冷热测试使用的联合 release 构建。CLI 也可单独安装。

| 范围 | 包数 | release 可执行文件大小 |
|---|---:|---:|
| niri-clip | 100 | 5,890,104 B |
| niri-clip-core | 83 | — |
| niri-clip-gui | 204 | 9,596,256 B |
| workspace | 222 | — |

```sh
for pkg in niri-clip niri-clip-core niri-clip-gui; do
  cargo tree --locked -e normal -p "$pkg" --prefix none | sed 's/ (\*)$//' | sort -u | wc -l
done
cargo tree --locked -e normal --workspace --prefix none | sed 's/ (\*)$//' | sort -u | wc -l
cargo build --locked --release -p niri-clip -p niri-clip-gui
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

原生选择器按需启动，默认关闭即退出。它使用 iced、winit 的常规 xdg 窗口及 tiny-skia 软件渲染。niri 根据 app-id `niri-clip-gui` 应用浮动、位置和阴影规则。winit 负责输入法事件。[选型 ADR-001](ADR-001.md) · [窗口规则示例](../assets/niri-clip.kdl)

**后端选择（`tui_backend`）**：`auto` 优先 native → fzf → fuzzel；
`gui_binary()` 三级兜底定位二进制（`PATH` → 与 CLI 同目录 → `~/.cargo/bin`），
因 niri `spawn` 环境的 PATH 常缺 `~/.cargo/bin`（真机踩坑，否则 auto 会误降级）。
显式 `"native"` 但二进制缺失时降级 fzf/fuzzel。fzf 版本地板 0.71
（`--id-nth`）在 `run()` 内二次校验。

**单实例与唤醒**。GUI 持有 `state/gui.lock` 的 flock，进程退出即释放。启用待机时，持锁者在私有的 `XDG_RUNTIME_DIR` 监听 Unix socket。地址由锁文件的设备号和 inode 决定，各测试库可独立使用。CLI 收到 GUI 事件循环的确认后返回。端点不可用时走常规启动，已有窗口可经 niri IPC 拉回焦点。

**短时待机**。`gui_idle_timeout_secs` 大于零时，正常关闭会销毁窗口，放下历史列表、搜索状态和缩略图引用。窗口关闭事件启动一次性定时器。期限内再打开会重读配置、历史与当前项，创建新的 xdg 窗口。窗口 ID 隔离旧窗口的异步结果和超时事件。期限届满时退出 GUI 并释放端点。字体库和分配器可能在待机时保留内存。[待机实测](#gui-standby)

**选择器配置**。CLI 和 GUI 每次打开都从完整配置提取菜单字段，沿用原有的类型校验和默认值。相同打开过程中的首批列表复用已读取的排序设置。之后重新聚焦仍会重读配置和历史。捕获过滤器留给入库与诊断路径编译。[分段复测](#selector-config)

**浮窗位置**。正常关闭前通过 niri IPC 读取本进程浮窗的输出名和逻辑坐标，变更时原子替换 `state/window-position.json`。有上次记录时，短时线程在窗口登记后核对输出，并按窗口 ID 相对移动。坐标取自工作区视图，用差值移动可兼容面板留白与小数缩放。第一次打开、换输出、平铺窗口或禁用 `remember_window_position` 时使用合成器规则。

**渲染与预览**。列表使用单行预览，底部窗格显示多行文本或图片。GUI 使用一个 Tokio 工作线程调度 iced 事件订阅，SQLite、复制和图片解码由按需创建的阻塞线程执行，最多同时运行四项。每次解码一张，完成后检查当前选中项。缩略图保留原比例，宽度上限为 1024 像素，高度上限为 440 像素，窗口缓存最近解码的四张。复制直接读取原图文件。单张源图解码仍需要与原始分辨率相应的临时内存。[image 缩略图接口](https://docs.rs/image/0.25.10/image/enum.DynamicImage.html#method.thumbnail)

历史行等高。原生窗口用滚动偏移与视口高度计算需要布局的行，并在上下用等高占位保留滚动范围。普通列按控件类型更新行与占位的状态，跳过视口外的绘制。行事件携带完整结果中的索引，悬停按当前坐标计算。首屏直接派发选中图片的解码任务，文本和空库无需启动消息来触发重新布局。

**搜索**。点击输入框或按 `/`、`Ctrl+F` 开始搜索。输入时先显示当前窗口的子序列匹配，同时后台查询 SQLite。三字符起走 FTS5 trigram，短词走 LIKE。同一时间只运行一个数据库查询，输入变化后接续最新请求。结果返回时核对输入和历史列表版本，仅使用仍匹配的结果，并重新定位选中条目。空历史、查询中和无匹配各有对应提示。[GNOME 搜索交互规范](https://developer.gnome.org/hig/patterns/nav/search.html)

`Tab` 在搜索框与结果之间切换焦点，保留查询。顶部显示选中项在结果中的位置。行高亮同时使用颜色和下划线，文本预览有独立的键盘滚动操作。冷启动和待机重开共用按键路由，搜索剪切及自动重复事件不会触发删除。

| 行为 | fzf | 原生窗口 |
|---|---|---|
| 初始列表 | 最近窗口，最多 300 条 | 最近窗口，最多 300 条 |
| 搜索范围 | 载入的条目 | 全库候选，最多显示 300 条 |
| 固定条目 | `★` | `◆` |
| 删除确认 | 列表行尾提示 | 窗口横幅 |

两个前端共用 `core::confirm` 的删除确认和 `preview::preview_multiline` 的文本截断规则。[交互决策 ADR-005](ADR-005.md)
