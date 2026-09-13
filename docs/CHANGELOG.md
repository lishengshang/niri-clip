# Changelog

## Unreleased

### Changed
- **默认 `ignore_regex` 强化（3.1）**：新增密码管理器输出格式的精确匹配——
  1Password 秘密引用 `op://`、OTP 迁移 `otpauth[-migration]://`、Bitwarden
  `bitwarden://`（scheme 词边界 + `://` 锚定，普通 URL 不误伤）、KeePassXC
  占位符 `{REF:`/`{TOTP}`/`{TIMEOTP}`；原关键词 `password|secret|token|otp|auth`
  保持子串语义不收窄（regex crate 无 lookahead，加词边界会把 password123
  这类真实密钥放过去——误报代价是少存一条，漏报代价是明文落盘）。命中
  链路本就静默（不落盘/不通知/不动 ▶ 指针），本任务零行为回归。已知
  边界：管理器复制的裸密码无格式特征，正则不可辨，兜底见 ROADMAP 3.3
  `wipe --sensitive`。**用户已有自定义 `ignore_regex` 的不受影响**（仅默认值
  变化）；`config.toml.example` 与 README 示例同步为 TOML 单引号字面量
  （`\b` `\{` 在双引号串里会被 TOML 转义规则损坏）。单测覆盖主流管理器
  格式 + 原关键词语义不回归 + scheme 边界不误伤

## v0.6.0 - 2026-09-12

### Fixed
- **AUR 安装后 systemd 单元不可用（打包契约，任务 2.6 A1）**：单元随二进制内置
  （`assets/niri-clip.service`）供 `cargo install` 用户走 `niri-clip install-service`，
  ExecStart 写的是 `%h/.cargo/bin/niri-clip`；而 `PKGBUILD` 原样装到
  `/usr/lib/systemd/user/`，AUR 二进制实际在 `/usr/bin`。systemd **不使用 `$PATH`**
  （非绝对路径只按编译期固定目录 `/usr/local/bin`、`/usr/bin` 解析），故 AUR 用户
  `systemctl --user enable --now niri-clip` 必然 `status=203/EXEC`——直接击穿
  「paru -S 后 Mod+V 开箱即用」这条唯一的最终用户路径。修复：`PKGBUILD`（及 `.git` 包）
  在 `package()` 内改写单元路径；源文件与单测保持 cargo 语义不变
- **`cargo publish` 依赖声明修复（打包契约，任务 2.6 A2）**：workspace 依赖
  `niri-clip-core` 补 `version`。此前只声明 `path`，cargo 打包时直接拒绝
  （"all dependencies must have a version requirement specified when publishing"），
  使 README 的 Cargo 安装与 ROADMAP 4.3 不可达。已用 `cargo package` 正反验证
- **fzf TUI 搜索全失（P0，任务 2.6）**：`--nth=5..` 的字段下标按 `--with-nth`
  **变换后**的行计算（man fzf："calculated against the transformed lines ...
  because fzf doesn't allow searching against the hidden fields"），而
  `--with-nth=1,2,3,5..` 隐藏 id 后只剩 **4** 列 → `--nth=5..` 没有任何字段参与
  匹配，**输入任意查询列表即被清空**（实测 fzf 0.74 `--filter` 零命中）。
  改为 `--nth=4..`；抽出 `FZF_NTH`/`FZF_WITH_NTH`/`FZF_ID_NTH` 常量并补行格式
  不变式（`debug_assert` 校验输入列数 = 原始列数）；两条回归测试锁定：
  口径一致性断言（隐藏列数 → `--nth` 下标）＋ 本机真实 fzf 端到端匹配。
  注：`{4}` 占位符与 `--id-nth` 走**原始**行下标，原本就是对的，未改
- **GUI 搜索结果错位、且 `max_items` 设小会越界 panic（P0，任务 2.6）**：
  `App::filtered()` 的搜索候选链分支产出的是 `search_hits` 的下标，却被拿去
  索引 `clips`——两者是**顺序与内容都不同**的两个 `Vec<Clip>`。后果：查询
  ≥3 字符时显示变成"列表前 N 条"而非真正命中，Enter/Ctrl-Y 会复制到非命中项；
  且 `max_items < SEARCH_LIMIT`（例如用户设 100）时 `clips[i]` 越界 panic。
  修复：过滤逻辑抽为纯函数 `compute_filtered`，返回值携带"下标所属来源"，
  `FilteredCache` 把来源与下标一起缓存，取条目改用 `get()` 兜底。
  4 条单测锁定（下标空间 / 候选超窗口不 panic / 过期候选回落 / 空查询原序）
- **GUI 搜索态无快选序号**：行首快选序号仅空查询时渲染（`query.is_empty()`
  门控），搜索态整列消失但 Alt+1-9,0 快选实际可用——序号改为任意状态显示，
  并补上第 10 行从未渲染过的 `0`（与 update.rs 快选分支语义对齐）
- **GUI 搜索态无快选**：裸数字快选仅空查询时生效，进入 `/` 搜索后数字被
  持焦点输入框接管、快选彻底不可用——新增 Alt+1-9,0 快选（任意时刻可用，
  含搜索态；0 = 第 10 行），空查询裸数字路径排除 Alt 修饰避免双触发

### Changed
- **`tui` 模块从 core 迁至 CLI crate（任务 2.6 D1）**：恢复 ARCHITECTURE 自定的分层
  ——core 是纯逻辑库、不含 UI 后端选择，fzf/fuzzel 编排与终端探测属 CLI 职责；
  原生 UI 不再为 fzf 编排代码买单。CLI 因此新增 `dirs` 直接依赖（Cargo.lock +1 行）
- **入库与收尸逻辑去重（任务 2.6 C6/C7）**：文本/图片两条入库路径共用 `upsert_clip`
  骨架（BEGIN IMMEDIATE → 查重 → 刷 ts 或插入 → 提交 → 刷当前项指针），差异收敛到
  `on_new_row` 回调；`enforce_max_items` 与 `prune_before` 共用
  `delete_rows_image_paths` 收集待删数据文件
- **`niri-clip delete` 对 ★ 条目不再弹 fuzzel 弹窗（任务 2.6 C1 / ADR-005）**：
  改为统一状态机的"挂起 + 提示 15 秒内再次执行"。无 fuzzel 环境下 ★ 条目此前
  "删不掉"（只能 `--force` 绕过，确认机制形同虚设），现在正常可用。
  **行为变化**：非 `--fzf` 路径首次调用会打印 `pending: ...` 且条目保留
- **CI lint 工序补 `--locked`（任务 2.6 A4）**：与 test/build/bench 三道工序口径一致。
  此前缺它则 lint 可在依赖漂移的树上通过，而 release 工序失败
- **CI bench 门禁收敛为循环 + 补"bench 必须出现"断言（任务 2.6 D12）**：三段近乎
  相同的 `awk` 合并；并新增断言——某 bench 名未出现在输出中即报错，否则改名一个
  bench 会让门禁**静默失效**
- **`niri-clip status` 输出可读化（任务 2.6 D9）**：不再打印整个 `Config` 的 Debug
  表示（含 `ignore_re: Some(Regex(...))` 内部结构），改为逐项列出条目上限 / 后端 /
  预览与图片开关 / 通知 / 体积上限
- **配置默认值单一来源（任务 2.6 D5）**：`Default::default()` 与 serde 的各
  `default_*()` 此前各自硬编码同一批字面量（改一处忘另一处即漂移）；现统一取新增的
  `defaults` 模块常量，并加单测断言"空 TOML 的反序列化结果 == `Default`"
- **`store` 测试改为目录布局（任务 2.6 D11）**：`src/store/{mod.rs,tests.rs}`，
  去掉 `#[path = "store_tests.rs"]` 的非惯用写法
- **MSRV 显式声明（任务 2.6 D7）**：`[workspace.package] rust-version = "1.75"`
  （与 README 承诺一致），让 cargo 给出明确报错而非编译期乱码
- **`migrate` 去掉一层 shell（任务 2.6 D8）**：`cliphist decode` 的 id 改走 stdin
  管道，不再用 `sh -c "echo {id} | cliphist decode"` 做字符串拼接
- **`chafa` 渲染尺寸提为常量（任务 2.6 D10）**：与 `preview_width` 语义不同
  （终端字符网格 vs 列表行宽），独立常量并注释区分
- **`PKGBUILD.git` 补齐 man 与 shell 补全（任务 2.6 B13）**：与主包对齐
- **列表窗口统一为 300（任务 2.6）**：fzf TUI 与原生 UI 此前取不同窗口
  （300 vs 全量 `max_items`＝默认 750），同一份历史两个后端看到的条目数不一致。
  现统一由 `store::MENU_LIMIT` 定义（原 `TUI_LIMIT` 更名，并与语义独立的
  `SEARCH_LIMIT` 并列注释）；GUI 删除自有常量 `MAX_RENDER_ROWS`。
  为不损失搜索面，GUI 的搜索门槛从 ≥3 字符降到 **≥1**：<3 字符由
  `store::search` 既有的 LIKE 全库回退承担（覆盖面不降反升）
- **`StoreStats` 移除 `text_bytes` 字段（任务 2.6）**：全仓库无消费方
  （仅一处测试断言），顺带少一次全表 SUM 查询

### Added
- **大库长稳测试（2.5）**：新增 `crates/niri-clip-core/tests/large_db.rs`，100k 条
  规模下覆盖写入 / 查询 / 并发 / 维护 / 迁移五段，逐条对齐本任务的验收口径——
  **无锁死**（每段上报耗时，外层 `timeout` 兜底）、**无数据丢失**（每段条目数
  断言；迁移段进一步断"只减不增 + 精确合并数 + 指纹全局唯一 + 抽检
  hash == blake3(text) + FTS 仍能命中幸存行"）、**内存平稳**（200 轮
  list+search 下 RSS 不得膨胀；迁移峰值 RSS 单独上报）。迁移段照 `migrate.rs`
  的 v1/v2/v3 步骤**自建 v3 旧库**（含 500 组"同文本不同旧 hash"的重复行，
  其中一条被并行带星标，用于验证合并时 pinned 取 OR），这是外壳脚本做不到、
  而 `migrate.rs` 注释里明确挂账给 2.5 的压力验证。**默认 `#[ignore]`**——
  100k 规模耗时以分钟计，不进 PR 门禁（门禁仍是 71 个快速用例）；规模可由
  `NIRI_CLIP_STRESS_N` 覆盖，便于二分定位。实测数值见 ARCHITECTURE §9。
  另在 CI 增加**手动触发**的 `stress` job（`workflow_dispatch`）供需要时在干净
  环境跑一遍——不进 PR 门禁，分支保护要求的六道工序不受影响
- **删除确认状态机（任务 2.6 C1 / ADR-005）**：新增 `niri-clip-core/src/confirm.rs`，
  ★ 条目二次确认的**唯一实现**（15s TTL，状态落盘 `state/pending_delete`）；CLI 与
  原生 UI 只负责呈现挂起态。判定分两层——`decide()` 纯 fs 判定（供长驻 UI 在消息
  路径安全调用，不碰 sqlite），`request()` = decide + 立即删除（供一次性 CLI 进程）。
  6 条单测覆盖：非星标直删 / 星标两段 / 过期重新挂起 / 挂起目标转移 / 损坏状态文件 /
  `decide` 不自行删数据。收敛前该语义有**三套分叉实现**（fzf 15s TTL、GUI 内存 bool
  无 TTL、CLI fuzzel 弹窗）
- **单实例互斥（任务 2.6 C4）**：新增 `niri-clip-core/src/single_instance.rs`，
  `InstanceGuard::try_acquire(name)` 统一 flock 机制；GUI 侧原有的 PID 文件 +
  `/proc/{pid}/cmdline` 复核机制删除（flock 由内核在崩溃时释放，无陈锁、无 PID 回收
  造成的假阳性）
- **菜单行渲染唯一出口（任务 2.6 C2）**：`render_row()` 统一 fzf 初始输入 /
  `list-raw` / `search` 三处的 5 列格式。此前该格式在 4 处各拼一遍——列格式变更要改
  4 处、挂起标记只在其中 2 处生效
- **多行预览下沉 core（任务 2.6 C5）**：`preview::preview_multiline()` 成为 CLI
  `preview` 与原生 UI 底部窗格的唯一实现（此前两处各自实现，且 GUI 用字节长度比较
  判断截断，与"逐行按字符截断"口径不匹配，会漏判）
- **历史导出/回灌（2.4）**：`export <file|->` 全量导出 NDJSON——首行 header
  （format/version/user_version/exported_at/count）+ 每行一条目，图片载荷内嵌
  base64 自包含单文件（image_path 本机绝对路径不导出），按 ts ASC 稳定排序
  （同库两次导出可 diff），不受 max_items/TUI_LIMIT 限制，文件权限 0600；
  `--sqlite <路径>` 额外用 `VACUUM INTO` 产出完整 db.sqlite 物理快照（已存在
  拒绝覆盖）。`import [--dry-run]` 回灌合并：hash 幂等（已存在跳过且不刷新
  ts——import 不是捕获，不打扰 ▶ 时序），每条重算 hash 完整性校验（文本
  blake3 / 图片 FNV key），损坏条目跳过并警告、好条目照常入库；单 BEGIN
  IMMEDIATE 事务原子提交，图片文件 tmp+rename 与行插入同窗口；保留原 ts 与
  pinned、不触碰 ▶ 指针，结束按当前配置执行一次 enforce_max_items。格式为
  Phase 5「历史内容动作插件化」扩展点，选型与被拒备选见 ADR-004；7 个新
  单测锁定往返/幂等/ts 保留/损坏拦截/max_items 裁剪/格式 schema/物理快照；
  依赖闭包：serde_json（GUI 原有直接依赖，CLI 经 core 引入 +4）+ base64
  （零传递依赖 +1），ARCHITECTURE §9 已同步
- **FTS5 全库搜索（2.1）**：schema v2→3 建 `clips_fts` 外部内容表 + 三触发器同步 + 存量回填（旧库升级无损，单测锁定）；tokenizer 选型 **trigram**（中英文任意子串均命中，推翻 ROADMAP 原定的 unicode61 起步，被拒备选与代价边界见 ADR-002）。`store::search`：≥3 字符走 MATCH 短语 + bm25 相关度，<3 字符退化为 LIKE 线性扫描（通配符转义、MATCH 查询引号翻倍转义）；GUI 搜索接入全库 MATCH——后台线程取候选（(query, gen) 双新鲜度缓存，过期丢弃不闪烁）+ fzf 风格评分重排保持 UX 一致；CLI 新增 `search <query> [--limit N]` 子命令（输出同 list-raw 5 列格式）；fzf TUI 内嵌过滤保持 fzf 自身模糊匹配。基准 `fts_search_300_of_10k` 实测 ≈0.16ms（预算 <50ms 的 1/300），bench 已入本地基准设施（进 CI 门禁待另行批准）
- **文本 hash 统一为 blake3（2.2）**：schema v3→4 一次性全表重算——
  DefaultHasher 跨编译器/进程不稳定（rustc 升级即变），存量库已存在
  "同文本不同 hash"的翻倍形态隐患，v1.0 数据可携（任务 2.4 导出/回灌）
  的硬前置。迁移事务（BEGIN IMMEDIATE + 事务内重读版本防双进程竞态）
  内按 blake3(text) 分组合并重复（幸存行 = ts 最大，pinned 取 OR，
  image_path 继承），DELETE 经 FTS 触发器自动同步；迁移前 `VACUUM INTO`
  快照 `state/db.sqlite.pre-blake3`（快照失败即放弃迁移），迁移后重映射
  ▶ 当前项指针；防翻倍断言单测锁定（条目数只减不增 / 文本行全量重算 /
  图片 FNV 指纹不动 / FTS 同步 / 幂等）。选型与被拒备选见 ADR-003；
  依赖闭包 +3 crate（零传递依赖的 blake3 栈，ARCHITECTURE §9）
- **数据统计与维护命令（2.3）**：`stats`（条数/星标/图片条目计数，库体积 =
  db.sqlite + -wal（-shm 为瞬态共享内存不计），图片体积 = images/ 目录磁盘
  实测，图片占比与最旧/最新条目日期）；`vacuum`（VACUUM 重建库文件 +
  `wal_checkpoint(TRUNCATE)`，断开连接后测"后"体积，返回前后对比——daemon
  常驻连接下并发写在途时按 busy_timeout 等待，不中断服务）；`prune --before
  <YYYY-MM-DD>`（按本地时区日历日零点切分，非 UTC——否则边界条目多删/少删
  8 小时；星标与当前项 ▶ 受保护，与图片 GC（1.3）保护语义一致；`--dry-run`
  仅预览不动数据；行删经 FTS 触发器同步索引，图片数据文件随行删除，残留由
  孤儿清扫兜底；SUM 统计与 DELETE 同处 BEGIN IMMEDIATE 事务防并发失真）。
  5 个新单测锁定保护语义/dry-run/FTS 同步/日期解析/体积口径；CLI 新增 chrono
  直接依赖（原为 core 传递引用，闭包零增量，ARCHITECTURE §9）

### Removed
- CLI `delete` 的 fuzzel 弹窗确认路径（约 30 行，见 Changed）
- GUI 的 PID 文件实例锁与 `/proc` 复核（见 Added）
- `StoreStats.text_bytes` 字段（全仓库无消费方，顺带少一次全表 SUM）

## v0.5.2 - 2026-09-01

> 亮点：Phase 1「TUI 体验闭环」收官（PRIMARY 捕获、图片配额 GC、★ 删除
> 二段确认、man page 与补全、criterion 基准进 CI）；依赖与构建开销审计
> 两轮收窄——iced/image feature 收窄 + 桌面通知换 notify-send 子进程，
> CLI 主包闭包 171→108 crate、release 编译 96s→77s、二进制
> 6.5→5.1 MiB，编译时间预算重估为 <120s；GUI 背景闪烁根治与切换
> 条目跳动三轮修复；**运行时依赖新增 libnotify**（AUR depends 已同步）。

### Added
- **★ 条目删除 fzf 内嵌二段确认（1.5，去 fuzzel 依赖路径）**：fzf TUI
  删星标条目改为两次 Ctrl-X——首次仅挂起（state/pending_delete 记
  id+时间戳，15s TTL），list-raw reload 后该行预览尾追 "◆
  再按Ctrl-X确认删除" 标记，同行再按才真删；按在别的 ★ 行挂起转移，
  过期自动作哑防分心误删，fzf 启动清残留。AUR 主包（仅 CLI，fzf TUI
  即主界面）从此无 fuzzel 也能删 ★；CLI `delete --fzf` 旗标承载此
  逻辑，原 fuzzel 确认路径保留（供手动键绑定用户）；新增二段确认
  全流程单测。附带修复：store/config/tui 三处测试各自持有独立 XDG
  环境锁并行时互相踩踏（新增测试暴露）——统一为共享全局锁
- **PRIMARY selection 捕获（1.1）**：新配置 `capture_primary`（默认关，
  划选噪声大）。开启后 daemon 拉起双 watcher（剪贴板 + `wl-paste --watch
  --primary`），鼠标划选的文本也入库（中键粘贴语义）；主选区与剪贴板
  同一去重空间（先划选后复制同内容只留一条），▶ = 最后成功捕获；每次
  捕获仍被 timeout 划界；watcher 参数单测覆盖 --primary 开关语义
- **man page 与 shell 补全（1.7）**：新增 `niri-clip completions <shell>`
  （bash/zsh/fish/elvish/powershell）与 `niri-clip man` 子命令（均输出到
  stdout），PKGBUILD 由二进制自生成安装到 man1 与补全路径；生成器内部对
  EPIPE 会 panic（clap_complete shells/shell.rs），改先写内存缓冲再忽略
  错误输出，对齐 outln! 口径；依赖 clap_complete/clap_mangen（闭包 +3 crate）
- **图片磁盘配额 GC（1.3）**：新配置 `max_image_total_bytes`（默认 200 MiB，
  0 = 不限），daemon 启动时随孤儿清扫一并执行 `store::gc_images`：images/
  总量超配额按时间戳 LRU 整行淘汰最旧图片条目（行删文件也删），星标与
  当前项（≈ Ctrl+V 内容）受保护；可淘汰集合为空时宁超配额不丢数据
- **criterion 基准设施（1.6）**：新增 `crates/niri-clip-core/benches/store.rs`，
  覆盖 `list_300_of_10k`（端到端含 Config::load/connect）与
  `sqlite_select_300_of_10k`（裸查询）两组基准；种子经公开入库 API 写入
  临时 XDG 环境，不触碰真实历史库。实测 ≈0.95ms / ≈0.47ms，远低于
  ROADMAP 预算（11ms / 4ms）；criterion 仅 dev 依赖且裁掉 plotters/rayon
  特性（依赖闭包 +11 crate，审计结论见 ARCHITECTURE）；CI 第 6 道 bench
  工序（绝对预算断言）已接入
- **GUI 搜索改 fzf 语义（/ 进入搜索）**：`/` 或 Ctrl-F 才进入搜索态
  （输入框聚焦、光标亮起），Esc 退出并轮换输入框 Id 熄灭光标；未进入时
  字符不进搜索、导航/快选不受影响。附带收益：非搜索态窗口零强制重绘
  待机——光标闪烁是周期性背景闪烁的最后重绘源，从机制上移除

### Changed
- **桌面通知换 notify-send 子进程（1.8 决策项②）**：移除 notify-rust
  （86 crate 的 zbus/zvariant D-Bus 栈，CLI 主包最大单项）——核实实际
  API 面仅 summary/body（9 处调用），新增 `core::notify::send`（后台线程
  spawn+wait 收尸：调用方零阻塞、daemon 长驻无僵尸；notify-send 缺失
  静默与原 `let _ =` 语义一致；参数数组不经 shell 无注入面；经
  coreutils timeout 5s 划界（代码评审修复：防 D-Bus 会话总线异常时
  libnotify 默认 ~25s 超时导致 daemon 内线程/子进程无界堆积）），
  daemon/tui/CLI/GUI 全量替换；CLI 主包闭包 171→108 / GUI 300→247 /
  workspace 322→269，Cargo.lock 净 -592 行，release 编译实测主包
  96s→77s / GUI 增量 123s→108s，二进制 6.5→5.1 / 11.2→9.8 MiB；
  **运行时依赖新增 libnotify**（PKGBUILD* / .SRCINFO.example depends 已
  同步）；未来若需通知 action 回调需换回库方案；notify 单测 1 例
  （notify-send 缺失时调用面不 panic）
- **依赖与构建开销审计（1.8）**：iced `image` feature 换 `image-without-codecs`
  + 直接依赖 image 收窄 png/jpeg/webp（GUI 解码全部走后台预解码
  `load_from_memory` → `Handle::from_rgba`，iced 渲染器零解码）——28 个
  lockfile 包（avif/av1 编码栈、exr/gif/tiff/hdr 等冷门解码器与 rayon）出
  闭包，Cargo.lock -576 行，GUI 363→300 / workspace 385→322，CLI 主包不变
  （171，不含 gui）；审计基线（cargo tree 全量口径、大头分解、编译时间与
  二进制体积）记入 ARCHITECTURE §9：release 编译实测主包 96s / GUI 增量
  123s（clean+--locked+LTO），二进制 6.5 / 11.2 MiB；编译时间预算已重估为
  <120s（lto 维持全量），notify-rust→zbus（86 crate，CLI 最大单项）裁剪
  仍待用户决策，未实施
- **预览窗格全铺满**：去圆角/边框/阴影（SHADOW_PANEL 删除），列表与
  预览间零缝隙，PANEL 底色直达窗口边缘（旧卡片式样在圆角与缝隙处露出
  窗口底色，闪烁时观感更明显）

### Fixed
- **TUI 复制图片条目写占位文本（fzf/fuzzel 路径，代码审阅发现）**：两处
  手写 wl-copy 只灌 clip.text，图片条目会把 "[image …]" 占位顶进剪贴板
  并毁掉真实截图（v0.5.1 修过 CLI copy 的同类问题，TUI 路径漏改）——
  统一改走 copy_to_clipboard（图片按 mime --type 直灌 + 刷当前项指针），
  顺带消除两段重复的 wl-copy 手写代码；另修 fzf ctrl-y 绑定裸 niri-clip
  依赖 PATH（niri spawn 环境常缺 ~/.cargo/bin，会静默失效），与同函数
  其它 bind 一致改用 exe 全路径；preview_id 截断提示阈值（2000 字节）
  与实际截断口径（100 行×300 字符）不一致的误标一并修正
- **GUI 切换条目跳动（三轮根因）**：① 底部预览窗格三种分支高度不一
  （文本 220px/图片 260px/缺失提示随内容），切换时窗格跳变使列表重排——
  三种分支外层一律定高；② 键盘导航滚动时列表在静止指针下滑过，iced 按
  布局重算派发 on_move/on_enter，MouseMove 把悬停跟随重新打开后被
  Hover 抢走选中，高亮/预览在键盘位置与指针位置间震荡（快慢键速触发
  概率不同）——MouseMove 改由订阅层物理 CursorMoved 派发，不再挂
  widget 级 on_move；③ 图片预览同步解码卡 UI：tiny-skia 在 layout 阶段
  同步解码未缓存截图（数十至上百 ms，UI 线程冻结期间停在旧帧，解完
  突然出现，观感"像加载"）——改为后台线程预解码 RGBA（image crate，
  iced 已引入同版本零新增依赖），Handle::from_rgba 供渲染器零解码
  直用，解码前面板显示"解码中…"定高占位；RGBA 缓存 8→4 张（内存
  预算折中）；is_image_magic 魔数预判随之废弃（解码失败优雅降级）；
  ④ ▶ 指针 TTL 缓存（500ms）的刷新在 view 渲染路径同步读盘，而搜索框
  光标闪烁恰好以 ~2Hz 强制整窗重绘——背景以光标同节奏周期性闪烁
  （用户观察实锤）——IO 移出渲染路径：view 只读缓存，刷新仅在消息
  路径（refresh_cur）执行

## v0.5.1 - 2026-08-31

> 亮点：原生 GUI 重构为常规 xdg 窗口（ADR-001 修订 1，window-rule 约束 + 原生 IME
> 中文搜索）、全库搜索与 fzf 风格相关度排序、单实例保护、GUI 键鼠交互补齐；
> 热路径性能优化；一批数据生命周期与 GUI 正确性修复。设计决策见 ADR-001 与 docs/NATIVE-UI.md。

### Added
- **原生 GUI 交互**：鼠标悬停跟随选中、左键点击行复制关闭（对齐 Enter）、
  右键连复（对齐 Ctrl-Y）；搜索命中字符红色高亮（fzf hl 语义）；
  空查询 `0` 键快选第 10 行（1-9,0）
- **全库搜索与相关度排序**：GUI 搜索范围从最近 300 条扩到全库
  （max_items，默认 750），渲染上限 300 行兜底；命中结果按 fzf 风格
  评分排序（连续命中/词首加权 + 位置弱惩罚）
- **单实例保护**：Mod+V 连按不再多开——`state/gui.lock` 存 PID，
  活实例经 niri IPC 聚焦其窗口后自退；残留死锁自动覆写接管
- **GUI 重新聚焦刷新**：窗口重聚焦即重拉列表——daemon 在失焦期间捕获的新内容不再缺失；
  不滚动、选中按 id 重定位，浏览位置不受影响
- **图片文件孤儿清扫**：`store::prune_orphan_images` 回收 images/ 下无主
  数据文件（daemon 启动时执行一次），兼容旧版本存量残留与 `.tmp-` 崩溃残片
- **AGENTS.md AI 协作开发约定**：Git 写面（commit/push/PR/issue/发布链/系统级部署）默认请示制，
  每轮收尾输出"建议 Git 动作清单"由用户逐项决策
- 底部预览窗格可滚动（80 行 / 每行 300 字符），长文不再截断丢失
- 复制/固定/删除失败走桌面通知（`notify_enabled` 门控，false 保持静默）
- GUI 键盘导航滚动跟随：方向键把选中行滚进可视区中部（视口实测自适应），
  行间分界线，行定高 27px 保证滚动偏移精确
- config/preview 单元测试补齐：默认值/自定义正则/非法 TOML 回退/
  XDG 相对路径拒绝；预览截断（多字节字符对齐）/换行单行化/降级链
- GUI instance 模块：niri windows JSON 解析与 app_id 匹配单元测试

### Changed
- **原生 UI 架构修订（ADR-001 修订 1）**：layer-shell 覆盖层改为常规 xdg
  窗口（app-id = `niri-clip-gui`）——可被 niri window-rule 全量约束（悬浮/
  位置/边框/阴影由用户 rule.kdl 约定）；winit 原生 IME 解锁中文搜索；
  底部预览窗格直接渲染剪贴板图片（iced image widget）
- **原生 UI 视觉重做**：JetBrainsMono Nerd Font 等宽 + 深色配色统一；
  `剪贴板> ` 提示符式搜索行（去输入框边框）；提示行键位双色高亮 +
  右上过滤计数（fzf header 风格）；圆角选中高亮、面板阴影立体化、
  交互式滚动条（悬停/拖动才浮现）；窗口 760x420 → 500x675 左上浮层，
  assets/niri-clip.kdl 补 window-rule 示例
- **渲染器固定 tiny-skia 纯软件**：NVIDIA wgpu 冻结（上游 #360）与 GL
  启动失败双问题的彻底规避，二进制 -1/3，与显卡驱动解耦
- **热路径性能**：`ignore_regex` 编译产物随 `Config::load` 缓存（不再
  每条入库重复 `Regex::new`）；`insert_with`/`insert_image_with` 复用
  调用方配置（一次捕获 3 次读盘解析降为 1 次）；GUI `filtered()` 结果
  按（列表代数, 查询）缓存，悬停/选中/复制等高频事件不再重算全库评分；
  tokio 特性 `full` 收敛为 `rt/rt-multi-thread/macros/time/process`
- **打包与遗留清理**：PKGBUILD.git 刷新 0.5.0 基线（去 fuzzel/nirius
  硬依赖、补 -flto 剥离）、`.SRCINFO.example` 同步、`config.toml.example`
  与代码默认值对齐；instance.rs 改 `niri msg -j` JSON 解析（附测试）、
  PID 复核收紧为 argv[0] 精确匹配；`Clip.ts`/`legacy_cliphist_db` 等
  死代码清理；config/preview 内联测试补齐
- GUI 图片预览遵循 `enable_image_preview` / `enable_preview` 配置
- GUI 组件化：main.rs（1060 行）拆分 theme/search/instance 模块，
  search 附评分/标记/大小写口径单元测试
- manual.sh 修正为 5 列 list-raw 格式（id 第 4 列），加临时 XDG 环境隔离
  不再触碰真实历史库

### Fixed
- **GUI Ctrl-X 删错行/跳顶（真根因，E2E 实锤）**：搜索框持有焦点时 iced text_input
  把 Ctrl+X 当剪切处理，空输入无编辑也发 `on_input("")` 且先于按键订阅到达，
  Query 处理器无条件 `set_selection(0)` 使删除执行时选中已归零——表现为永远删掉
  顶部行、高亮跳顶。现同值 Query 回调直接忽略；选中改为按 clip id 跟踪
  （重载后 `relocate_selected` 按 id 重定位，防 daemon 捕获/固定操作重排行序
  导致高亮漂移）；星标二段确认随选中移动自动取消，防确认残留误删下一行
- **图片条目复制写占位文本**：`copy_to_clipboard` 对图片条目把
  "[image mime N bytes]" 占位文本顶进剪贴板（并毁掉真实截图）——现按 mime
  以 `wl-copy --type` 灌入 `images/{id}.bin` 文件字节
- **图片数据文件生命周期闭环（P1）**：delete/wipe/超限淘汰同步删除
  `images/{id}.bin`（RETURNING 带出路径），新增 `prune_orphan_images`
  孤儿清扫（daemon 启动执行，兼容存量残留与 `.tmp-` 崩溃残片）；
  `insert_image` 写文件纳入事务窗口（`.tmp-` 先落盘再原子 rename），
  杜绝"有行无图"导致 hash 占用该图无法重录
- **图片条目必崩**：`images/{id}.bin` 扩展名无法被 `Handle::from_path`
  识别 → tiny-skia 渲染线程 panic "Image should be allocated"；
  改按字节内容解码 + 位图魔数门控（非图片数据回落缺失提示）
- **图片每帧重复解码**：`Handle::from_bytes` 每次生成新 Id 导致
  tiny-skia 缓存失效；按 clip id 跨帧 LRU（8 项）缓存，命中刷新顺序
- **CLI SIGPIPE panic**：`niri-clip status | head` 等管道截断时 println! 写入
  EPIPE 直接 panic（Rust 默认 SIGPIPE=SIG_IGN）——改用忽略写失败的 outln!
- fuzzel 路径 `wl-copy` 补 `Stdio::null()`（对齐 fzf 路径防黑屏残留）
- GUI 后台任务 panic 按任务类型回传兜底消息（Copy panic 走失败通知
  不被静默吞掉）；选中/快选/导航以可见行数为界
- 键盘滚动到底时选中态闪烁：列表滚过静止指针逐行触发 on_enter
  抢走选中；键盘导航期间暂停悬停跟随，真实移动恢复
- 符号 tofu 方框：❯▶◆⏎ 等字形缺失（Noto Sans Mono + fallback 失败），
  主字体指定 JetBrainsMono Nerd Font；`↵`（系统级缺字形）统一替换 `⏎`
- clippy 警告归零；xdg 迁移后 text_input/scrollable 落回浅色默认主题的
  割裂观感（全部控件自定义深色样式）

## v0.5.0 - 2026-08-28

> 亮点：原生 layer-shell UI（tui_backend=native，无终端秒开）、▶ 当前项置顶、
> 单条体积限流。详见 docs/NATIVE-UI.md 与 ADR-001。

### Added
- **tui_backend 新增 `native` 后端（M5.4）**：`niri-clip tui` 在
  niri-clip-gui 可用时拉起原生 layer-shell 窗口（无终端、秒开），
  `auto` 优先 native、缺二进制自动降级 fzf/fuzzel；显式 `native`/
  `fzf`/`fuzzel` 可锁定后端。Mod+V 绑定无需改动
- **当前项置顶与 ▶ 标识**：新概念"当前项"= 最后一次成功捕获的内容 ≈
  `Ctrl+V` 会粘出的东西。`store` 捕获成功（含去重刷 ts 路径）即刷新
  `state/current` 指针；`list()` 排序把当前项固定在第 1 行（星标之上）；
  fzf/fuzzel 行首打 `▶`，与 `★` 可叠加（`▶★`）。`copy` 子命令与 TUI
  Enter/Ctrl-Y/fuzzel 选中路径同步刷新指针，会话内 reload 的 ▶ 跟随移动。
  当前内容被 ignore_regex 过滤或超限时 header 提示"当前剪贴板不在历史中"。
  `migrate` 导入旧历史前保存、结束后还原指针，避免 ▶ 误指
- **单条体积限流（路线图 P1-2）**：新配置 `max_clip_bytes`（默认 1 MiB）与
  `max_image_bytes`（默认 10 MiB）。store 层守卫覆盖所有入库调用方
  （daemon 三个捕获路径 / migrate）超限拒绝并桌面通知；
  捕获读取改 `Read::take(max+1)` 有界读，读取过程内存上限即限额，
  杜绝 `read_to_end` 对超大载荷的全内存直通
- native 回退轮询对超限内容以内容 hash 短路，避免每 500ms 重复通知
- 单元测试 4 例：文本/图片在限额边界的入库与拒绝行为、
  当前项指针跟踪/置顶/超限过滤不移动指针

### Changed
- **TUI 启动提速与关闭闪窗修复**：`run()` 的 tty 探测提到最前，
  niri spawn 拉起的外层进程不再白跑 `fzf --version`；终端模拟器探测
  优先级调整为 foot > ghostty > kitty（终端冷启动是 Mod+V 链路主要
  延迟，ghostty 明显轻于 kitty）；承载 fzf 的内层命令输出重定向到
  `~/.local/state/niri-clip/tui.log`——fzf 退出后 scrollback 不再闪现
  启动日志/copied 文本，日志文件兼作无 systemd 环境的排障入口；
  `fzf --version` 门控结果缓存到 `state/fzf.version`（按 fzf 二进制
  mtime 自动失效重校），高频路径再省一次子进程
- **Cargo workspace 拆分**（Phase 5 前置 5.0）：niri-clip-core（业务库）
  + niri-clip（CLI）+ niri-clip-gui（原生 UI）；移除未使用的 serde_json

### Fixed
- CI smoke 适配 `list-raw` 5 列格式（num/▶/★/id/preview，id 列移位），
  并新增 ▶ 置顶语义断言（当前项压过星标、pin 落第 2 行）

## v0.4.1 - 2026-08-27

> 分支 `fix/issue-2-daemon-reliable-capture`（堆叠于 PR #1 分支之上）。
> 事故：daemon 进程存活但捕获停摆 19 分钟——轮询 `read_to_end` 对个别
> 来源应用永久阻塞且无错误输出。

### Fixed
- 捕获架构重构为**事件驱动优先**：`wl-paste --watch` 在 selection 变化时
  把载荷直灌 `niri-clip store` 的 stdin；每次捕获子进程经
  `timeout ${capture_timeout_secs}s` 划界，任何病态读挂起都会按秒级回收。
  该故障形态在机制上被消除（不再存在常驻循环等待单一阻塞读的结构）
- 非 UTF-8 剪贴板载荷不再以 lossy 形式污染文本库（显式跳过并记日志）

### Changed
- native 500ms 轮询降级为**回退模式**：仅在系统缺失 `wl-paste` 时启用；
  其文档明确标注丢帧窗口/空闲开销/长阻塞风险三项取舍
- 图片抓取从轮询循环迁移到 `store` 的空 stdin 探测分支
  （文本失败→受开关约束的图片 MIME 探测），同样受 timeout 边界保护

### Added
- `delete --force/-f`：跳过星标 GUI 确认的无头删除路径；无 fuzzel 的环境下
  交互式删除星标不再静默空转，改为显式提示并指引使用 --force（PR #2 评审项：
  CI smoke 的 pin→delete 断言即因此失败）
- 新配置项 `capture_timeout_secs`（默认 5s）
- `niri-clip install-service` 一键安装内置 systemd user 单元模板并给出启用指引；
  单元文件补充 Documentation 与 flock 双开说明
- GitHub Actions CI 五道门禁：fmt check / clippy -D warnings / test --locked /
  release build --locked / XDG 隔离 CLI 冒烟（store/list/pin/delete/wipe 断言）

## v0.4.0 - 2026-08-27

> 分支 `fix/issue-1-p0-correctness-store-daemon`：全面评估报告 P0 问题闭环。

### Fixed
- **图片预览错位**（P0）：预览不再"取 images 目录最新一张"。schema 引入
  `PRAGMA user_version` 迁移机制（v2），新增 `clips.image_path` 列，
  数据文件按 clip id 写 `images/{id}.bin` 并精确关联渲染
- **并发静默丢数据**（P0）：SQLite 加 `busy_timeout=5000`；
  SELECT 去重检查 + INSERT 收进 `BEGIN IMMEDIATE` 事务原子化，
  消除 fzf 选择旧条目时 wl-copy 与 daemon 并发插入同 hash 的 UNIQUE 冲突竞态；
  daemon/store 的入库错误改为显式记录，不再被 `let _ =` 吞掉
- **daemon 启动探测 panic**（P0）：探测改为单次 `get_contents` 并 match
  ClipboardEmpty/NoMimeType/NoSeats 三类良性错误；旧实现第二次调用
  `unwrap_err()` 在剪贴板恰好可用时直接 panic，systemd 下表现为周期崩启
- **图片等长误判重**：内容指纹改为 FNV-1a64 + mime + 字节长度
  （旧版仅 mime+len，两张等大 PNG 只会收录第一张）
- **TUI 后端门控**：auto 后端不再强制要求 kitty 存在才启用 fzf，
  foot/alacritty 用户恢复 track/reload/pin/delete 完整能力
- 配置路径 fallback 硬编码 `/home/mio/.config` 移除；
  header 提示 "Enter粘贴" 更正为事实行为 "Enter复制"

### Changed
- **数据库迁至 `~/.local/state/niri-clip/db.sqlite`**（XDG state 规范）：
  `~/.cache` 会系统清理工具误删整份历史；首次连接用 `VACUUM INTO`
  一致性快照自动搬迁旧库，旧库保留备份；状态目录 0o700、库文件 0o600
- **复杂度偿还**：移除进程内 200ms 缓存层（fzf reload-sync 每次 spawn 新进程，
  该缓存从未在 reload 路径生效）与未参与任何查询的 FTS 占位表；
  菜单取数统一 `list(min(max_items, TUI_LIMIT))`
- `preview_text` 先廉价截断（O(width)）再换行替换，大文本 reload 不再全文扫描 ×300 行
- 图片数据目录随库迁至 `~/.local/state/niri-clip/images/`；旧孤立时间戳图片不迁移

### Added
- daemon 单实例 flock 锁（state 目录 `daemon.lock`），双开立即报错退出
- 单元测试基础设施：XDG 环境变量隔离临时目录，覆盖去重原子性 /
  busy_timeout 生效 / schema 版本迁移 / 图片关联与内容判重 / 旧库快照搬迁 /
  pin 排序与 limit（共 6 例）
- `Cargo.lock` 入库（PKGBUILD `cargo build --locked` 前置条件）

### Removed
- `store::bench_10k` 死代码；FTS 同步相关的手工维护语句随占位表一并删除

## v0.3.0 - 2026-08-27

### Added
- daemon 原生 `wl-clipboard-rs` 轮询 (500ms)，不再 `fork wl-paste`，失败自动回退到 `wl-paste --watch`
- store 懒加载 `TUI_LIMIT=300` + 200ms 缓存，`invalidate_cache` 在 insert/delete/pin/wipe 时失效
- `store::bench_10k()` 10k 条压测，实测 `list 300 <11ms` / `sqlite 10k <4ms`
- tui `chafa` 图片预览：`enable_image_preview=true` 时 `preview` 尝试 `chafa --format symbols`
- `tests/manual.sh` 自动造 20 条验证删除后 `pos` 跟随 + 压测 + 图片配置检查

### Changed
- `config.toml.example` 默认 `enable_image_preview=true`
- `tui::list_raw` 处理 `Broken pipe` (head -n5) 不 panic，`writeln` 忽略错误
- `store::list` 新增 `list_tui()` 缓存层，`tui` 自动切 300 条

### Fixed
- `list-raw | head` 导致的 `Broken pipe` panic

## v0.2.0 - 2026-08-27

### Added
- Rust 重写：`config` / `store` / `daemon` / `tui` / `preview` 模块
- `~/.config/niri-clip/config.toml` 配置化 (max_items, preview_width, ignore_regex, tui_backend)
- `niri-clip daemon` 守护进程：`wl-paste --watch niri-clip store` → SQLite WAL
- `niri-clip tui` 单进程 fzf `--track --id-nth 2` + `reload-sync`，删除不跳顶，支持 `fuzzel` 回退
- `niri-clip {store,list-raw,preview,pin,delete,wipe,migrate,status}` 子命令

### Changed
- `clipboard-history-ui.sh` 自动切 Rust TUI (有 `niri-clip` 就用 `niri-clip tui`)
- `clipboard-history.sh` 自动切 Rust daemon
- 分支 `master` → `main`

### Fixed
- `Mod+V` 删除后跳回顶部的问题 (Plan B)
- `config.toml` 容错：缺字段时回退默认值

## v0.1.0 - 2026-08-27

- Bash Plan B：单进程 fzf + track + id-nth 修复跳顶
- 脚本部署到 `~/.config/niri/scripts/`
- 项目初始化
