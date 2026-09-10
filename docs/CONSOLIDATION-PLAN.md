# 收敛与精简方案（v0.6 前置整顿）

> 基准日期：2026-09-10 · 代码基线：`c5ed430`(main) · 版本：0.5.2
> 范围：**不进入任务 2.5（大库长稳测试）**，先对全项目做一次体检 → 做减法 → 补标准性。
> 本文是**临时工作文档**，批次全部落地后删除或折叠进 ROADMAP。

---

## 一、结论摘要

**底子是健康的，问题不在 bug 在"熵"。**

实测门禁（本次全量复跑，`rustc 1.98.1`）：

| 门禁 | 结果 |
|---|---|
| `cargo fmt --check` | 干净 |
| `cargo clippy --all-targets -- -D warnings` | 零警告 |
| `cargo test --locked` | 58 通过（core 53 / gui 5），0 失败 |
| `TODO/FIXME/XXX/HACK` | 代码与文档中**零残留** |

所以本轮不该去找"哪里写错了"——而应解决三类**结构性熵**：

1. **对外契约断链**：两条 v1.0 承诺（`paru -S` 开箱即用 / `cargo install`）在构建配置层就是断的，与代码质量无关；
2. **真相源失真**：文档从"记忆载体"退化成"误导来源"——ROADMAP/ARCHITECTURE/README/NATIVE-UI 四份文档互相矛盾，且都滞后于代码事实；
3. **同语义多实现**：同一件事（删除确认、行渲染、单实例、多行预览、上限常量）存在 2–3 套实现，且**已经出现语义分叉**。

三项里第 1 项是"必须修"，第 2 项是"修了立刻回本"，第 3 项是"再不收敛就会持续长出分叉"。

**交付范围 = 38 项全量，零遗留。**（E 组为执行期间新发现的代码缺陷，见 §二） 问题清单里的 P2/"可延后"只表示执行排序靠后，**不表示可以不做**：结构冗余、文档失真、标准性三类问题只要留下尾巴，下一轮就会以"新一轮体检"的名义重新被发现一次——重复发现的成本远高于当场收干净。收尾标准见 §四「DoD 零遗留」。

---

## 二、问题清单

### A 组 · 对外契约断链（P0，必修）

**A1. AUR 用户 `systemctl --user enable --now niri-clip` 必然失败**

- 证据：`assets/niri-clip.service:8` → `ExecStart=%h/.cargo/bin/niri-clip daemon`；
  而 `PKGBUILD:38` 把该单元**原样**装到 `/usr/lib/systemd/user/`，AUR 二进制却在 `/usr/bin/niri-clip`。
- 事实核实：systemd **不使用 `$PATH`**，非绝对路径只按编译期固定目录（`/usr/local/bin`、`/usr/bin`…）解析，`%h/.cargo/bin` **不在其列**。故 AUR 用户启动服务得到 `status=203/EXEC`。
- 影响：直接击穿 ROADMAP §一「最终交付形态：`paru -S niri-clip` → `Mod+V` 开箱即用，零手工配置」，且是本项目**唯一**面向最终用户的安装路径。
- 附带：`daemon.rs:432` 的单测 `embedded_service_unit_points_to_cargo_bin` 反过来把 cargo 路径锁成了"契约"，会阻止直接改源文件。
- 建议动作：`PKGBUILD` 的 `package()` 内对单元做 `sed`（`%h/.cargo/bin/niri-clip` → `/usr/bin/niri-clip`），源文件与单测保持 cargo 语义不变。零新增文件、双路径各自正确。

**A2. `cargo publish` / `cargo install niri-clip` 在当前 workspace 配置下不可用**

- 证据：`Cargo.toml:17` → `niri-clip-core = { path = "crates/niri-clip-core" }`，**只声明 path 未声明 version**；`crates/niri-clip/Cargo.toml:17` 与 `crates/niri-clip-gui/Cargo.toml:11` 均走该 workspace 依赖。
- 机制：cargo 发布时无法把 path 依赖写进 registry（"all dependencies must have a version specified when publishing"），`cargo publish` 直接拒绝。
- 影响：ROADMAP 4.3 与 README「Cargo 安装」段落当前是**不可达承诺**；GUI 同理（README:37 还承诺了 `cargo install niri-clip-gui`，而它是**完全没进发布计划**的 crate）。
- 建议动作：workspace 依赖补 `version = "0.5.2"`（后续随 release bump 同步）；用 `cargo publish --dry-run -p niri-clip-core` 验证链路。此项**只改构建元数据、不改行为**。

**A3. `tests/manual.sh` 会清空用户真实的 cliphist 历史**

- 证据：脚本头部自称「全程在临时 XDG 环境隔离运行，**绝不触碰真实剪贴板历史库**」，但第 22 行执行 `cliphist wipe`。`cliphist wipe` 作用于 `~/.cache/cliphist/db`——不受 `XDG_*` 隔离影响（niri-clip 自己的 `wipe` 走的是隔离后的 `XDG_STATE_HOME`，是安全的；cliphist 那行不是）。
- 影响：本仓库唯一的手工测试脚本，任何人为了"验证"跑一次就会毁掉自己真实的剪贴板历史。这是**运维级事故面**，且与脚本自身的承诺正相反。
- 建议动作：删除该行（真要清理 cliphist 应显式显式化并二次确认）；同时该脚本第 5 段自称"压测 10k"但实际只插 200 条，与 README「20 条 pos 跟随 + 压测」的描述同样失真——见 B5。

**A4. 门禁口径不齐（CI 与文档约定不一致）**

- 证据：`.github/workflows/ci.yml:25` 是 `cargo clippy --all-targets -- -D warnings`，**缺 `--locked`**；同文件 test/build/bench 三处都有 `--locked`。而 `AGENTS.md` §二 与 `ROADMAP` §十四.6 都写的是 `cargo clippy --all-targets -- -D warnings`（同样缺）。
- 影响：lint 工序可在依赖漂移的树上通过，release 工序却会失败——门禁语义不统一。
- 建议动作：lint 工序补 `--locked`，同步修正 AGENTS.md 与 ROADMAP 的门禁命令行。

### B 组 · 文档真相源失真（P1，修了立刻回本）

AGENTS.md §六 的精神是"文档即 AI 的记忆"，而当前记忆已系统性失真。逐条给证据：

| # | 失真点 | 证据 | 与事实的差 |
|---|---|---|---|
| B1 | README 版本徽章停在 0.5.0 | `README.md:6` `version-0.5.0` | 实际 0.5.2 |
| B2 | README「路线」整段滞后一个 Phase | `README.md:196-201`：v0.5 标"▶"（进行中）、v0.6 列为未来 | Phase 1 早已交付（v0.5.2 tag 在）；Phase 2 的 2.1–2.4 **已完成** |
| B3 | README 配置示例与默认值自相矛盾 | `README.md:90` `enable_image_preview = true` | `config/config.toml.example:8` 与 `config.rs:91` 均为 **false**（默认关） |
| B4 | ARCHITECTURE §3 有**损坏段落** | `ARCHITECTURE.md:103-105`：`（子串语义）` 之后直接跳到 `仅 dev 依赖、裁掉 plotters/rayon 特性，闭包 +11 crate）。` | 中间一句被吃掉，句子不成文；且 "+11 crate" 与 `Cargo.toml:56-58` 注释矛盾 |
| B5 | README「测试」段承诺的脚本行为不存在 | `README.md:187` 「20 条 pos 跟随 + **压测**」；`manual.sh` 第 5 段只插 200 条 | 无 10k 压测；见 A3 |
| B6 | `.SRCINFO.example` 版本错位 | 头部 `pkgver = 0.5.2`，但 `source` 指向 `v0.5.1.tar.gz`（`.SRCINFO.example:19`） | 发布物与示例不一致 |
| B7 | `config.toml.example` 头部版本标注过期 | `config/config.toml.example:1` 「niri-clip **v0.4** 配置」 | 文件内容已跟到 v0.5.2（含 capture_primary），只有标题没跟 |
| B8 | **NATIVE-UI.md 状态头与勾选项互相矛盾** | `NATIVE-UI.md:3` 「状态：已立项，**未排期**——执行窗口默认在 v1.0 GA 之后」；同文件 §5 的 M5.2/M5.3/M5.4 大量 `[x]`，而前置任务 `5.0 core 下沉` 仍是 `[ ]` | GUI crate 已存在并交付（`crates/niri-clip-gui/` 7 文件 1538 行）；"未排期"与"已验收"不能同时成立 |
| B9 | ROADMAP 自相矛盾 + Phase 标题未跟 | `ROADMAP.md:61` 「✅ Phase 1 …已交付」vs `ROADMAP.md:91` 「## 四、Phase 1 …（**进行中**）」；`ROADMAP.md:181` 「原生 UI … 执行窗口 v1.0 GA 之后」 | 与 B8 同一根因：GUI 的实际进度从未回写路线图 |
| B10 | ARCHITECTURE 与 ROADMAP 的**依赖/编译数字互不一致** | ARCHITECTURE §9 表：CLI 112 / core 92 / GUI 250 / ws 272；ROADMAP 开销表：CLI 108 / GUI 247 / ws 269。同一轮审计，两套数 | 且 notify 替换的收益在三个地方写成三个数：ARCHITECTURE `-63`、`notify.rs:4` `-81`、ROADMAP `86`（原闭包） |
| B11 | ARCHITECTURE 整章未覆盖 GUI | `ARCHITECTURE.md` §4 标题「TUI - 不跳顶」只写 fzf/fuzzel 后端 | 全项目 1/4 代码量（GUI crate）在"原理"真相源里无位置；§1 架构图同样只有 fzf 一条路径 |
| B12 | 整个 `docs/PLAN.md` 已被取代 | 全文 57 行：数据位置写 `~/.cache/niri-clip/db.sqlite`（v0.4 已迁 state）、「FTS5 待 v1.0 搜索」（2.1 已完成）、「`migrate` 仍保留，之后可删」 | 与 ARCHITECTURE/ROADMAP 全面冲突，最后修改停在 2026-08-27 |
| B13 | PKGBUILD.git 与 PKGBUILD 安装面不一致 | `PKGBUILD` 装了 man + bash/zsh/fish 补全；`PKGBUILD.git` 全无 | git 包用户没有 man/补全 |

**建议动作（成组处理）**：B1–B7 是一轮机械校准（当轮完成）；B8/B9/B10/B11 需要一次"事实回写"（见 §四 批次 1）；B12 建议**删除**；B13 对齐。

### C 组 · 同语义多实现与分叉（P1，做减法主体）

**C1. 「星标条目删除确认」有三套实现，且语义已经分叉**

| 实现位置 | 机制 | 有效期 | 落盘 |
|---|---|---|---|
| `core/tui.rs:156` `delete_with_fzf_confirm` | `state/pending_delete` 文件 + list-raw 行尾标记 | **15s TTL** | 是 |
| `gui/update.rs:322` `delete_selected` | App 内存字段 `confirm_delete: bool` | **无 TTL**（永久挂起直到再按/移动/Esc） | 否 |
| `cli/main.rs:173-203` `delete`（非 `--fzf`） | 现场 spawn `fuzzel --dmenu` 两选项弹窗 | 无 | 否 |

- 同一产品动作，三条代码路径三种语义。GUI 与 fzf 版的差异是**用户可感知**的（挂起后去接个电话回来，GUI 仍处于"再按即删"状态，fzf 版已作废）。
- 且三处都与文档不一致：README:164 只写了「星标条目二段确认」一种描述。
- 建议动作：把确认状态机收敛为 core 的单一 API（含 TTL 语义参数），CLI/GUI 各自只做"呈现"（fzf 行尾标记 / GUI 横幅 / fuzzel 弹窗）。**语义选型需你拍板**（见 §五 决策 3）。

**C2. 5 列行渲染循环在 core 里重复 4 次**

- 证据：`tui.rs:322-331`（run_fzf）、`455-461`（run_fuzzel）、`502-518`（search_raw）、`531-548`（list_raw）四处各自拼 `num/▶/★/id/preview`，`row_marks` 是共用的但拼装逻辑没有。
- 影响：任何列格式变更要改 4 处；B8 提到的行尾标记（pending）只在其中 2 处生效。
- 建议动作：抽 `write_rows(out, clips, cur, pending)` 单一出口。

**C3. 上限常量三套同名 300 + 载入窗口不一致**

- 证据：`store.rs:13` `TUI_LIMIT=300`、`store.rs:600` `SEARCH_LIMIT=300`、`gui/main.rs:374` `MAX_RENDER_ROWS=300`，三个独立 const 同为 300、语义不同却无交叉引用。
- 更实质的问题：TUI 载入 `min(max_items, 300)`（`tui.rs:101`），GUI 载入**全量** `max_items`（`gui/main.rs:217-222`，默认 750）。**同一份历史，两种后端看到的条目窗口不同**，且无任何文档说明。
- 建议动作：core 内定义一组具名常量并注明各自语义；把"TUI 300 窗口 vs GUI 全量"作为**显式决策**记入文档（要么统一，要么写明理由）。

**C4. 单实例锁两套机制**

- 证据：core `daemon.rs:289` 用 `flock`（`state/daemon.lock`）；GUI `instance.rs:9` 另建 PID 文件（`state/gui.lock`）+ `/proc/{pid}/cmdline` 复核。
- 说明：GUI 的**需求确实不同**（连按 Mod+V 要"聚焦已开窗口"而非拒绝启动），所以不是纯粹的重复。但"实例互斥"这一底层机制应来自 core，GUI 只应提供"已存在时做什么"的回调。
- 建议动作：core 暴露 `try_single_instance(name) -> Option<Guard>`，GUI 复用，`instance.rs` 的 119 行可降到 ~40 行。

**C5. 多行预览格式化两套**

- 证据：CLI `tui.rs:568-584`（100 行 / 每行 300 字符 / 截断提示）与 GUI `view.rs:261-272`（80 行 / 每行 300 字符 / `↵→⏎`）。
- 注：GUI 的**行内单行预览**走的是 core 的 `preview::preview_text`（正确复用）；重复的是**底部多行窗格**那段。
- 附带缺陷：`view.rs:268` 用 `clip.text.len() > out.len()`（字节）判断是否补 `…`，与逐行按字符截断的口径不匹配，会漏判。
- 建议动作：core `preview` 增加 `preview_multiline(clip, max_lines, line_width)`，两处共用；参数从硬编码提为常量。

**C6. 入库骨架重复**

- 证据：`store.rs:152-207`（`insert_with`）与 `221-276`（`insert_image_with`）共享同一套「BEGIN IMMEDIATE → SELECT 去重 → UPDATE ts / INSERT → touch_current → enforce_max_items」骨架，重复约 50 行。
- 建议动作：抽 `upsert_row(conn, hash, text, mime, ts, size, cfg) -> UpsertResult`，两处各自处理"图片文件写入"这一差异点。

**C7. 落行删文件的"取路径→删行→删文件"模式重复 3 处**

- 证据：`store.rs:286-294`（enforce_max_items）、`363-381`（gc_images）、`517-542`（prune_before）各自实现 RETURNING image_path 的收尸逻辑。
- 建议动作：抽 `delete_rows_returning_image_paths(conn, sql, params) -> Vec<String>`。

### D 组 · 结构与规范（P2）

| # | 问题 | 证据 | 建议 |
|---|---|---|---|
| D1 | **`tui` 模块住在 core，违反自身分层约定** | `core/lib.rs:13` `pub mod tui`；而 `NATIVE-UI.md:78-80` 明确写 core = "store/config/daemon 逻辑下沉（**纯 lib，无 UI**）"，`niri-clip/` = "CLI + TUI"。经查 core 内部**无人引用** `tui`（唯一消费者是 `crates/niri-clip/src/main.rs` 8 处） | 把 `tui.rs` 移到 `crates/niri-clip/src/`。收益不止分层：GUI 将不再链接 fzf/fuzzel/终端探测/版本门控等纯 CLI 逻辑 |
| D2 | `StoreStats.text_bytes` 是死字段 | `store.rs:424` 定义、`437` 查询，全仓库仅 `store_tests.rs:629` 断言使用；CLI 输出（`main.rs:211-232`）从不展示 | 删字段 + 删查询（顺带少一次 SQL） |
| D3 | GUI 三处 `std::process::exit(0)` | `gui/update.rs:118`、`213`、`instance.rs:30` | 绕过析构：sqlite 连接、图片 Handle、iced 运行时均不释放。改用 `iced::exit()` |
| D4 | `enforce_max_items` 未显式保护当前项 | `store.rs:280-297` 只排除 `pinned=0`；而 `gc_images`（`store.rs:366`）与 `prune_before`（`store.rs:487`）都显式排除 `hash != current` | 当前是**隐式**依赖"当前项 ts 必为最新"这一不变式。建议补注释 + 一条单测锁定，或统一保护语义（三处保护口径不一致本身就是熵） |
| D5 | 配置默认值存在两条真相 | `config.rs:53-82` 的 `default_*()` 与 `config.rs:84-105` 的 `Default::default()` 各自硬编码同一批字面量（750/100/1/正则/1MiB/10MiB/200MiB…） | 让 serde 缺省经 `Default` 取值（如 `serde(default)` + 自定义 wrapper），或加一条"两处必须一致"的单测。当前值恰好一致，但没有机制保证下次一致 |
| D6 | AGENTS 原则 2 与实际口径冲突 | `ROADMAP.md:85` 原则「错误不得静默吞掉（`let _ =` 禁令）」；实际生产代码 `let _ =` **27 处** | 这些多为 best-effort 清理（删临时文件/写指针/通知），并非"吞掉业务错误"。建议**把原则精确化**：区分"清理类可忽略"与"业务错误禁止忽略"，而不是逐处改造 |
| D7 | MSRV 未声明 | README:7 承诺 Rust 1.75+，`Cargo.toml` 无 `rust-version` | `[workspace.package] rust-version = "1.75"`，让 cargo 给出明确报错而非编译乱码 |
| D8 | `migrate_from_cliphist` 走 shell 拼接 | `store.rs:774-776` `sh -c "echo {id} | cliphist decode"` | `id` 来自 `parse::<i64>` 故无注入面，但可改 `Stdio::piped()` 直接喂 stdin，去掉一层 shell |
| D9 | `status` 打印整个 Config 的 `{:?}` | `main.rs:319` `outln!("config: {:?}", cfg)` | 含 `ignore_re: Some(Regex(...))` 的 Debug 输出，既难读也无用。建议只列关键项 |
| D10 | `render_preview` 尺寸硬编码 | `preview.rs:46` `"60x20"`，与 `preview_width` 配置无关且未文档化 | 提为常量或接入配置 |
| D11 | 测试文件布局非惯用 | `store.rs:797` 用 `#[path = "store_tests.rs"] mod tests;`，`store_tests.rs` 1064 行平铺在 `src/` 下 | 可改 `src/store/` 目录布局（`mod.rs` + `tests.rs`）。纯结构偏好，收益有限，可延后 |
| D12 | CI bench 断言三段复制粘贴 | `ci.yml:100-105` 三个几乎相同的 `awk` 块 | 收敛为一个循环 |

### E 组 · 代码缺陷（执行期间新发现，均为 P0，已修）

> 来源：任务 2.6 的两路代码审计 + 人工复验。原计划的 36 项偏"结构与文档"，
> 实际动手后额外挖出 2 个**功能性缺陷**，两者都直接损坏核心可用性。

| # | 缺陷 | 证据与机制 | 状态 |
|---|---|---|---|
| E1 | **fzf TUI 搜索全失**：输入任意查询，列表被整体清空 | `tui.rs` 的 `--nth=5..` 配 `--with-nth=1,2,3,5..`。man fzf 明确 `--nth` 按 `--with-nth` **变换后**的行计下标，隐藏 id 后只剩 4 列 → 无字段参与匹配。实测 fzf 0.74.3 `--filter` 零命中（`--nth=4..` 正常命中） | 已修 + 2 测试（口径断言 + 真实 fzf 端到端） |
| E2 | **GUI 搜索结果错位，且 `max_items < 300` 时越界 panic** | `gui/main.rs` 的 `filtered()`：搜索候选链产出 `search_hits` 的下标，却拿去索引 `clips`（两个顺序与内容都不同的 `Vec<Clip>`）→ 显示"列表前 N 条"而非命中，Enter 复制到非命中项；候选数超过列表窗口时 `clips[i]` 越界 | 已修 + 4 测试（下标空间/超窗口/过期候选/空查询） |

**已驳回的误报（记录在此，避免下一个 agent 重复排查）**：

| 误报 | 为何不成立 |
|---|---|
| "`prune_orphan_images` 回收不了 rename 后的孤儿 `{id}.bin`，永久泄漏" | 不成立。跳过条件是 `!name.starts_with(".tmp-") && referenced.contains(..)` **两者同时满足**；未被引用的 `{id}.bin` 不命中跳过条件，会被正常删除。`store.rs` 原注释是对的 |
| "三处保护当前项/星标的实现语义不一致" | 不成立。`enforce_max_items` 只保护 pinned 是**有意为之**——当前项指针只在捕获时刷新，其 `ts` 恒为最新，不可能被当作"最旧"淘汰；另两处（`gc_images`/`prune_before`）显式排除 current |
| "GUI `preview_text` 与 core `preview::preview_text` 重复实现" | 部分不成立。GUI **行内**预览确实复用了 core 的多行窗格是另一需求（可滚、多行）；真正重复的是它与 CLI `tui::preview_id` 的多行截断逻辑（C5 已记录） |

---

## 三、减法账本

| 动作 | 净变化（行） | 性质 |
|---|---|---|
| 删 `docs/PLAN.md` | −57 | 纯删除 |
| `docs/NATIVE-UI.md` 收敛（保留未完成项，迁入 ROADMAP Phase 5） | −90 左右 | 纯删除 |
| 删 `.SRCINFO.example`（`makepkg --printsrcinfo` 可再生成，且已错位） | −24 | 纯删除 |
| C2 行渲染合并 | −40 | 去重 |
| C6 入库骨架合并 | −25 | 去重 |
| C7 三处收尸逻辑合并 | −20 | 去重 |
| C4 单实例收敛到 core | −30（GUI）/+15（core） | 去重 |
| C5 多行预览下沉 core | −12（GUI）/+20（core） | 去重 |
| C1 删除确认三合一 | 净 −30 | 去重 + 消除分叉 |
| D2 删 `text_bytes` | −5 | 纯删除 |
| D12 CI awk 合并 | −4 | 去重 |
| **合计** | **约 −290 行**（6154 → ~5870，−4.7%） | |

**减法不是重点，"一个语义只有一处实现"才是重点。** 上表里 6 项去重的价值不在行数，而在于从此 A 改 B 跟着变——本轮已发现 3 处"改了这里忘了那里"的既成事实（C1 的 TTL、C2 的标记只在 2/4 处、C5 的字节/字符口径）。

---

## 四、执行批次

原则：**每批一个主题、独立可验证、互不依赖可乱序**；每批结束跑齐 `fmt + clippy(-D warnings) + test`，并**同轮同步文档**（AGENTS.md §六 映射表）。

| 批次 | 主题 | 内容 | 风险 | 依赖 |
|---|---|---|---|---|
| **0** | 对外契约修复 | A1（service 单元 sed）+ A2（依赖补 version）+ A3（manual.sh 危险行）+ A4（CI/文档门禁口径） | 极低（元数据与脚本，零行为变更） | 无 |
| **1** | 文档事实回写 | B1–B13；删 PLAN.md；NATIVE-UI.md 收敛；ARCHITECTURE 补 GUI 章 + 修损坏段 + 统一数字口径；ROADMAP 状态标记校准 | 零代码风险 | 批次 0 的 A1/A4 会改文档，建议在其后 |
| **2** | 语义收敛 | C1（确认状态机统一为 core 单一 15s TTL 实现，GUI/CLI 只做呈现——**属行为决策，落 ADR-005**）+ D5（配置默认值单源）+ D4（保护语义补齐与测试锁定）+ C3（三个 300 归一为具名常量 + 两后端统一 300 + GUI 搜索门槛降到 1 字符） | 中（C1 有用户可感行为变化） | 无 |
| **3** | 结构减法 | D1（tui 移出 core，恢复自定分层）+ C2/C5/C6/C7（去重）+ C4（单实例收敛，GUI 只留"已存在时的动作"）+ D2（删死字段）+ GUI 侧分层与常量收敛 | 中（纯搬移/去重，必须测试全绿） | 建议在批次 2 之后（C1 会先动 tui/GUI，避免二次搬运） |
| **4** | 标准性补齐（必做，非可选） | D3（`iced::exit` 替 `process::exit`）+ D6（精确化 `let _ =` 原则）+ D7（MSRV）+ D8（去 shell 拼接）+ D9（`status` 输出可读化）+ D10（预览尺寸常量）+ D11（测试文件布局）+ D12（CI awk 收敛）+ 新增的文档失效机制落地 | 低 | 无 |

**批次 2 的 C1 与批次 3 的 D1 存在顺序耦合**：C1 要改 `tui.rs`，D1 要搬 `tui.rs`。建议先语义后搬家，避免同一文件短期内两次大动。

### 收尾标准：零遗留（DoD）

本轮**不接受"已知问题留待后续"**。A/B/C/D/E 五组 38 项全部为交付范围，其中 D 组的"低收益、可延后"评价只表示**排序靠后**，不表示可以不修。收尾必须同时满足：

1. 38 项逐项有落地结果（修复 / 删除 / 明确驳回并写明理由，三者之一）；
2. `fmt + clippy --all-targets --locked -- -D warnings + test --locked` 全绿；
3. 文档与代码事实零冲突——用 `docs/` 全量对读一遍代码核对（批次 1 的机制，批次 4 再复核一次）；
4. 无"孤儿文档"：任何被取代/失效的文档要么删除，要么其内容已被唯一真相源吸收；
5. 同语义只剩一处实现——`grep` 可验证（如 `confirm_delete` 只剩一处定义、行渲染只剩一个出口）。

### 文档失效与真相源单一化（本轮新增的规范）

当前失真的根因不是"忘了更新"，而是**失效文档没有退役机制**：`PLAN.md` 被取代后原地留了 14 天，`NATIVE-UI.md` 完成度与状态头各说各话，`.SRCINFO.example` 停在旧版本。故本轮把"如何让文档失效"写成规范并落到 `AGENTS.md`：

| 情形 | 处置 |
|---|---|
| 文档被新文档取代 | **删除**。git 历史即归档；不得留"已废弃"残骸在 `docs/` |
| 文档部分内容失效 | 立即重写该文件，使其**整体**与事实一致（不允许局部打补丁式标注） |
| 计划类文档（ROADMAP/PLAN 型） | 只保留**一份**。任务完成后勾选，不新建平行计划文档 |
| 实验/立项类文档（NATIVE-UI 型） | 立项期建，**交付当轮**收敛：剩余项迁入 ROADMAP，原文件删除 |
| 本方案自身 | 批次全部落地后删除，剩余可追踪项迁入 ROADMAP |

**明确不做（本轮）**：
- 任务 2.5 大库长稳测试（你已指定）
- ROADMAP Phase 3/4 的任何功能
- 新增依赖（本方案零新增，唯一变动是补 `version` 字段）
- AUR 实际提交 / 版本 bump / tag（属发布链，按 AGENTS.md 只建议不执行）

---

## 五、已决策（2026-09-10，用户确认）

| # | 议题 | 决定 | 理由 |
|---|---|---|---|
| 1 | 三份失效文档（`PLAN.md` / `NATIVE-UI.md` / `.SRCINFO.example`） | **全部删除** | git 历史即归档。NATIVE-UI.md 的未完项（5.4.2 窗口启动延迟实测、5.4.3 兼容矩阵）先迁入 ROADMAP Phase 5 再删文件；`.SRCINFO.example` 版本已错位，且可由 `makepkg --printsrcinfo` 随时再生成 |
| 2 | ★ 条目删除确认的统一语义 | **统一 15s TTL + 落盘**（收敛为 core 单一状态机） | fzf 现有语义最安全：挂起 15s 自动作废，防"分心后回来误删"。GUI 现状的**永久挂起**是最危险的一种（隔天回来按一次即删） |
| 3 | GUI 收敛范围 | **全部纳入本轮**；载入窗口统一为 **300**；同时把 GUI 搜索门槛从 ≥3 字符降到 ≥1 | 两个后端行为一致、内存更省。门槛下调后短查询走 `store::search` 已内置的 `LIKE` 全库回退（`store.rs:633-650`），**覆盖面不降反升**，故统一 300 无功能损失 |
| 4 | 版本节奏 | **并入 v0.6.0**（本轮整顿 + 任务 2.5 一起收尾） | 不必为纯工程批次单独造版本号，CHANGELOG 只需一个章节 |

**决策 3 的连带影响**：`MAX_RENDER_ROWS`（GUI）/`TUI_LIMIT`/`SEARCH_LIMIT` 三个 300 需归一为 core 的具名常量；ROADMAP 开销表里"TUI 窗口 300"的表述要改为"两后端统一 300"。

### 本轮新增发现（原 B10 的根因）

B10 里"同一份审计三个数字"（CLI 112 vs 108、收益 -63 vs -81 vs 86）**不是记录笔误，而是指标定义本身有缺陷**：文档采用的计数命令是

```
cargo tree -e normal --prefix none | sort -u | wc -l
```

`cargo tree` 会给"在树中出现多次"的 crate 行加 `(*)` 后缀，于是 `serde v1.0.229` 与 `serde v1.0.229 (*)` 被 `sort -u` 当成两条 → **虚高**。实测本机该伪影规模：CLI **37** 处、core 29 处、GUI 156 处（旧文档的注释把它当成"+1"，严重低估）。

本次重测三种口径对账（2026-09-10，`--offline`）：

| 口径 | CLI | core | GUI | workspace |
|---|---|---|---|---|
| A 旧文档所用：`sort -u` 全行（含 `(*)` 伪影） | 117 | 96 | 252 | 275 |
| **B 采用：剥除 `(*)` 后去重 name+version** | **100** | **83** | **204** | **222** |
| C 仅去重 crate 名（会合并同名多版本） | 96 | 79 | 190 | 208 |

**决定采用口径 B**（同名多版本如 `syn` v2/v3、`rustix` 0.38/1.1、`calloop` 0.13/0.14 确属两次编译，不能合并），并把命令原文写进 ARCHITECTURE §9——口径可复现是"数字不再漂移"的前提。批次 1 顺带把这两份文档的重复表述去重：**明细只留 ARCHITECTURE §9，ROADMAP 只留预算阈值**，从结构上消灭"两处数字各说各话"的可能。

---

## 六、执行进度（38 项逐项追踪，DoD 要求零遗留）

| 批次 | 状态 | 说明 |
|---|---|---|
| 0 契约修复 | **已完成** | A1（PKGBUILD 单元路径改写，已实跑 sed 验证）/ A2（依赖补 `version`，已用 `cargo package` 正反验证）/ A3（manual.sh）/ A4（CI 补 `--locked`）/ B13（git 包补 man+补全） |
| 1 文档回写 | **已完成** | B1–B13 全部处理；三份失效文档已删除；依赖计数口径已修正并写入 AGENTS.md 的「口径即契约」规范 |
| 2 语义收敛 | **已完成** | E1/E2 两个 P0 缺陷已修（+6 回归测试）；C1（`core::confirm` 单一状态机，ADR-005）；C3（常量与窗口归一）；D5（配置默认值单源 + 一致性单测）；D4（保护语义注释 + 不变式单测） |
| 3 结构减法 | **已完成** | D1（`tui` 迁出 core）；C2（`render_row` 唯一出口）；C5（`preview_multiline` 下沉）；C6（`upsert_clip` 骨架）；C7（`delete_rows_image_paths`）；C4（`core::single_instance`） |
| 4 标准性补齐 | **已完成** | D2 / D3 / D6 / D7 / D8 / D9 / D10 / D11 / D12 全部落地 |

**批次 1 落地明细（可用于核对）**：

- 删除：`docs/PLAN.md`、`docs/NATIVE-UI.md`、`.SRCINFO.example`（内容已迁入 ROADMAP Phase 5 / ARCHITECTURE §10 / ADR-001）
- README：版本徽章 0.5.0→0.5.2；路线段补齐 Phase 1/2 真实状态；`enable_image_preview` 示例改回默认 `false`；安装段改为"当前请走源码/makepkg"，不再承诺未上架渠道；测试段改为如实描述 `manual.sh`
- ARCHITECTURE：§3 损坏段落重写（拆出「基准设施」条目）；§9 口径修正 + 依赖数改为唯一真相源；**新增 §10 原生 UI**（原 NATIVE-UI.md 的原理部分）；§1 补 `state/` 全文件清单、两条 UI 路径与分层边界（含分层债）；§4 修正与代码不符的 fzf 参数（`--id-nth 2`→`4`、3 列→5 列）与 auto 优先级；§5 补 `tui_backend` 四态
- ROADMAP：Phase 1 状态改「已交付」；新增任务 **2.6 工程收敛**；Phase 5 native UI 行改写为"核心已交付 + 三条收尾项"；开销预算表只留预算与达标状态（数值指向 ARCHITECTURE §9）；**修正被 ADR-002 推翻的"unicode61 起步"残留**
- 其他：`config.toml.example` 头部版本对齐；AGENTS.md 新增「### 4. 文档失效与退役」+「口径即契约」，门禁命令补 `--locked`；`tests/manual.sh` 删除危险的 `cliphist wipe` 并修正"压测 10k"的不实表述

**待办（阻塞项）**：`PKGBUILD`、`PKGBUILD.git`、`Cargo.toml`、`.github/workflows/ci.yml` 四份发布链文件的改动需用户明确批准。

**批次 2 已落地明细**：

- `crates/niri-clip-core/src/tui.rs`：`--nth=5..` → `4..`（E1）；新增
  `FZF_ORIGINAL_COLS`/`FZF_WITH_NTH`/`FZF_NTH`/`FZF_ID_NTH` 常量与行格式
  `debug_assert` 不变式；+2 测试
- `crates/niri-clip-gui/src/main.rs`：过滤逻辑抽为纯函数 `compute_filtered`
  并返回下标所属来源（E2）；`FilteredCache` 结构体替代元组缓存、连来源一起缓存；
  `filtered()` 取条目改用 `get()` 兜底；`load_limit()` → `store::MENU_LIMIT`；
  删除 `MAX_RENDER_ROWS`；搜索门槛 3 → 1；`visible_len()` 用 `MENU_LIMIT`；+4 测试
- `crates/niri-clip-core/src/store.rs`：`TUI_LIMIT` → `MENU_LIMIT`（补语义注释）、
  `SEARCH_LIMIT` 上移并列、删 `StoreStats.text_bytes`（D2）
- `crates/niri-clip-gui/src/view.rs`、`store_tests.rs`、`backup.rs`：随常量更名同步
- `docs/CHANGELOG.md`：Unreleased 补 2 条 `Fixed`（P0）与 1 条 `Changed`
