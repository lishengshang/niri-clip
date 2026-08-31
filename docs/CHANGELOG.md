# Changelog

## Unreleased

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
