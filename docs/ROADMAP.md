# niri-clip 长期开发路线图

> 基准日期：2026-09-11 · 当前版本：v0.5.2
> 时间估算假设：单人维护者，每周 8–12 有效工时；所有时间节点为**相对量**，按实际投入动态校准。
> 实测数值（依赖闭包/编译时间/基准）的唯一真相源为 `ARCHITECTURE.md` §9，本文不重复记数。

---

## 一、项目定位与总目标

**niri-clip** 是为 niri 合成器打造的全新、高性能、开箱即用的 Wayland 剪贴板历史管理器（独立实现，非 cliphist 包装）。

**产品原则（所有阶段任务的准入门槛）：**

1. **高性能**：常驻 <40MB、10k 条搜索 <50ms，指标进基准门禁
2. **低开销**：二进制小、编译快、依赖少——新增依赖必须过审计（用途唯一、无重传递依赖）
3. **快速**：捕获零空闲、TUI 秒开、删除/固定光标不跳
4. **简洁**：一个二进制、一个 SQLite 文件、一个 systemd 单元，无隐藏状态
5. **功能够用**：以"剪贴板历史管理"为边界，宁缺毋滥；非目标见下

**非目标（明确不做，防 scope creep）：**
- 不做云同步/网络服务（v2.0 也仅限加密导出 + 文件同步）
- 不做自建 GUI 框架前端（layer-shell 原生 UI 仅限调研立项）
- 不做 cliphist 运行时兼容（仅 `migrate` 一次性导入）
- 不做插件系统（`export --json` 输出即是扩展点）

**开销预算（跟踪项，随 CI 基准更新）：**

> 口径原则：**本表只记"预算与是否达标"，实测数值的唯一真相源是
> ARCHITECTURE §9**。此前两处各自记数，导致同一轮审计出现两套数字
> （112/108、250/247…），本轮收敛为单一出处。

| 指标 | 预算 | 状态 |
|---|---|---|
| 常驻内存 | <40MB | 达标（~40MB） |
| 10k 条 list | <11ms | 达标 |
| 10k 条 FTS 搜索 | <50ms | 达标（实测 0.16ms，`fts_search_300_of_10k`，trigram；CI 预算断言已纳入） |
| release 编译时间 | <120s（`--locked`；2026-09-01 重估，原 <60s 定于依赖树远小的早期） | 达标 |
| 依赖闭包 | 新增依赖须过审计（用途唯一、无重传递依赖）；`cargo tree` 口径见 ARCHITECTURE §9 | 达标（CLI 100 / core 83 / GUI 204 / workspace 222） |
| 后端载入窗口 | **两后端一致 300**（`store::MENU_LIMIT`；GUI 短查询走 LIKE 全库回退，故无功能损失） | 达标（2.6 已统一） |


**最终交付形态（v1.0 定义）：**
- `paru -S niri-clip` → `Mod+V` 开箱即用，零手工配置
- Rust + SQLite(WAL/FTS5) + fzf/fuzzel，常驻 <40MB，10k 条搜索 <50ms
- 完整文本/图片/PRIMARY selection 支持，敏感内容安全过滤
- 三渠道分发：AUR（release/git/bin）、crates.io、GitHub Releases
- 完整文档：man page、shell 补全、README/ARCHITECTURE/CHANGELOG 同步
- CI 门禁保障：fmt / clippy / test / build / 冒烟 / 性能基准六道工序

**技术栈基线（已定型，除非重大阻塞不更换）：**

| 层 | 技术 | 状态 |
|---|---|---|
| 语言/运行时 | Rust 1.75+ / Tokio | ✅ 定型 |
| Wayland | wl-paste --watch 事件驱动 + wl-clipboard-rs 兜底 | ✅ 定型 |
| 存储 | rusqlite + SQLite WAL，`user_version` 版本化迁移 | ✅ 定型（FTS5 待启用） |
| TUI | fzf ≥0.71 `--track --id-nth`，fuzzel 自动回退 | ✅ 定型 |
| 预览 | chafa / kitty icat | ✅ 定型 |
| 打包 | PKGBUILD + systemd user 单元 | ✅ 定型 |

---

## 二、路线总览

```
✅ Phase 0   v0.1–v0.4.1   骨架 → MVP → 优化 → P0 正确性闭环      已交付
✅ Phase 1   v0.5.x        TUI 体验闭环                          已交付（v0.5.2）
▶ Phase 2   v0.6          搜索与数据治理（FTS5/blake3 统一/GC）    主线全交付（2.1–2.6 ✅），剩 2.7（不阻塞 v0.6.0）
  Phase 3   v0.7          安全与隐私强化                         约 2–3 周
  Phase 4   v1.0          Production 正式发布                    约 3–4 周
  Phase 5   v1.x          生态与集成（原生UI核心已交付/waybar/OSC52） v1.0 后持续
  Phase 6   v2.0+         长期愿景（加密/跨合成器/原生UI增强）     远期
```

里程碑节点：**M1 = v0.5.0 发布** → **M2 = v0.6 搜索与数据治理** → **M3 = v0.7 安全版本** → **M4 = v1.0 Production GA**。v1.0 前每阶段结束发布正式 tag + AUR 更新；v1.0 后按需 minor/patch。

---

## 三、已交付基线（Phase 0，压缩归档）

| 版本 | 日期 | 交付 |
|---|---|---|
| v0.1 | 2026-08-27 | 单进程 fzf + reload-sync + track + id-nth（修复跳顶），项目骨架 |
| v0.2 | 2026-08-27 | Rust MVP：config/store(SQLite WAL)/daemon/tui/preview/migrate 全子命令 |
| v0.3 | 2026-08-27 | wl-clipboard-rs 原生轮询、300 条懒加载（list <11ms）、chafa 预览、manual.sh |
| v0.4 | 2026-08-27 | P0 正确性闭环：图片预览错位/并发丢数据/单实例锁/启动 panic/等长图片误判；库迁 `~/.local/state`；6 个单元测试 |
| v0.4.1 | 2026-08-28 | 事件驱动捕获（wl-paste --watch + timeout 划界）、图片捕获入 store、`install-service`、CI 五道门禁 |

事故复盘沉淀的工程原则（后续阶段沿用）：
1. 子进程必须有超时边界（capture_timeout_secs 模式）
2. **业务错误**不得静默吞掉（清理类 best-effort 除外）。"静默吞错"的边界是
   **后果**而非语法（2.6 精确化：原表述为笼统的"`let _ =` 禁令"，与代码中大量
   **有意**的 best-effort 调用冲突，导致原则不可执行）——删临时文件 / 写辅助
   指针（`state/current`、`state/pending_delete`）/ 发桌面通知这类"失败也不影响
   数据正确性与返回值语义"的动作可忽略错误（仅记 stderr）；凡影响用户数据、
   返回值语义或状态一致性的错误必须上抛
3. schema 变更必须走 `PRAGMA user_version` 迁移
4. 数据文件按 clip id 关联，不依赖 mtime 等间接状态

---

## 四、Phase 1 — v0.5.x：TUI 体验闭环（已交付，v0.5.2）

**核心目标：** 补齐 TUI 交互短板，使日常使用无功能缺口；建立性能回归防线。

**关键任务：**

| # | 任务 | 要点 | 验收标准 |
|---|---|---|---|
| 1.1 | ✅ PRIMARY selection 支持 | `capture_primary` 开关（默认关，划选噪声大）：daemon 双 watcher（`wl-paste --watch --primary`），主选区与剪贴板同去重空间，▶ = 最后成功捕获（中键粘贴语义自洽）；每次捕获仍被 timeout 划界 | 选中即捕获（开关开启时）；watcher 参数单测 |
| 1.2 | ✅ `max_clip_bytes` 上限 | **P0**：daemon/store 入库前限流，超限拒绝 + 通知提示，防 DB 膨胀与 `read_to_end` 全内存直通；图片同理（store 层守卫 + `Read::take` 有界读） | 单元测试覆盖边界值（已过）；超限载荷不落库不产生数据文件 |
| 1.9 | ✅ 当前项置顶与 ▶ 标识 | "当前项"（最后一次成功捕获 ≈ Ctrl+V 内容）经 `state/current` 指针跟踪，排序固定第 1 行（星标之上），行首 `▶` 与 `★` 可叠加；copy/Enter/Ctrl-Y 路径刷新指针；被过滤/超限时 header 提示缺席 | CLI 冒烟验证 ▶ 置顶压过 ★、copy 后 `▶★` 合并上顶（已过）；指针行为单测 2 例 |
| 1.3 | ✅ 图片磁盘配额 GC | `store::gc_images`：`max_image_total_bytes`（默认 200 MiB，0 不限）超限按 ts LRU 整行淘汰（行删文件也删），星标/当前项受保护；daemon 启动随孤儿清扫执行；单测覆盖淘汰顺序与保护语义 | GC 后预览不串图（按 id 关联，v0.4 已保障） |
| 1.4 | ✅ `notify_enabled` 开关 | 桌面通知可关——已核实 config/daemon/tui/gui 全链路门控（false 完全静默），`config.toml.example` 与 README 已同步 | 配置生效（已过） |
| 1.5 | ✅ 星标删除二段确认（fzf 内嵌，去 fuzzel 依赖） | 两次 Ctrl-X：首次挂起（state/pending_delete，15s TTL 防分心误删），list-raw reload 打 "◆ 再按Ctrl-X确认删除" 行内标记，同行再按真删；`delete --fzf` 旗标承载，原 fuzzel 路径保留；二段确认全流程单测（GUI 内嵌确认见 5.3.3） | 删除误操作率归零（单测锁定挂起/确认/过期语义） |
| 1.6 | ✅ criterion 基准进 CI | `crates/niri-clip-core/benches/store.rs`（list_300_of_10k ≈0.95ms / sqlite_select_300_of_10k ≈0.47ms，见 ARCHITECTURE）；CI 第 6 道 bench 工序：bencher 格式输出 + 绝对预算断言（11ms / 4ms），超限即红 | CI 输出耗时，回归 >20% 报警（绝对阈值先行） |
| 1.7 | ✅ man page + shell 补全 | `niri-clip man` / `niri-clip completions <shell>`（clap_mangen/clap_complete，闭包 +3 crate）；PKGBUILD 由二进制自生成安装到 man1 与 bash/zsh/fish 补全路径 | PKGBUILD 安装路径正确（本地验证输出） |
| 1.8 | ✅ 依赖与构建开销审计 | `cargo tree -e normal` 全量口径（ARCHITECTURE §9）：CLI 主包 171 / GUI 300 / workspace 322；大头 notify-rust→zbus 86（CLI 最大单项）、wl-clipboard-rs 44、iced 258（仅 GUI）；iced `image`→`image-without-codecs` + 直接依赖 image 收窄 png/jpeg/webp（28 包出闭包，GUI 363→300）；PKGBUILD 已确认 `--locked`；release 编译实测主包 96s / GUI 增量 123s，**超 <60s 预算**列入决策项 | 审计结论记入 ARCHITECTURE §9；编译时间进开销预算表（已同步） |

**技术要点：** fzf `--expect` 组合键；criterion + CI artifact 对比。
**时间节点：** 约 2–3 周；里程碑 **M1 = v0.5.0 tag + AUR 更新**。

---

## 五、Phase 2 — v0.6：搜索与数据治理

**核心目标：** 解决"快而不可搜"的短板——启用 FTS5 应用内搜索；文本 hash 统一为 blake3（v1.0 硬前置）；建立数据生命周期管理。

**关键任务：**

| # | 任务 | 要点 | 验收标准 |
|---|---|---|---|
| 2.1 | ✅ FTS5 全文搜索 | `user_version` v2→3：clips_fts 外部内容表 + 三触发器同步 + 存量回填；tokenizer 选型 **trigram**（中文子串可用，推翻 unicode61 起步计划，见 ADR-002）；`store::search`（MATCH + bm25，<3 字符 LIKE 回退）；GUI 全库搜索接 MATCH（后台线程 + (query,gen) 新鲜度缓存）、CLI `search` 子命令；fzf TUI 内嵌过滤保持 fzf 自身模糊匹配（300 行窗口） | 10k 条中英文搜索 <50ms（实测 0.16ms）；旧库升级无损（单测锁定） |
| 2.2 | ✅ 文本 hash 统一为 blake3 | DefaultHasher 跨编译器/进程不稳定，**v1.0 硬前置**（ADR-003）：`user_version` v3→4 全表重算 blake3，迁移事务（BEGIN IMMEDIATE + 事务内重读版本防双进程竞态）内合并重复（幸存行 = ts 最大，pinned 取 OR，image_path 继承）；迁移前 `VACUUM INTO` 快照（`state/db.sqlite.pre-blake3`，快照失败即放弃迁移），迁移后重映射 ▶ 指针；图片 `img:` FNV 指纹不动 | 迁移前后条目数只减不增（单测锁定，含幂等/FTS 同步/指针重映射）；跨重启/跨机器去重稳定；blake3 闭包 +3 crate 过开销审计（ARCHITECTURE §9） |
| 2.3 | ✅ 数据统计与维护命令 | `niri-clip stats`（条数/体积/图片占比；库 = db+wal、图片 = images/ 磁盘实测双口径，-shm 瞬态内存不计）、`vacuum`（VACUUM + WAL 截断，返回前后体积）、`prune --before <YYYY-MM-DD>`（本地日历日零点切分；星标/当前项受保护同 1.3；--dry-run 预览；BEGIN IMMEDIATE 防并发失真） | 用户可自助管理磁盘占用（5 个单测 + 真机冒烟） |
| 2.4 | ✅ 历史导出/备份 | `export <file\|->` 全量导出 NDJSON（首行 header + 每行一条目，图片内嵌 base64 自包含，ts ASC 稳定排序可 diff，文件 0600）；`--sqlite` 附 `VACUUM INTO` 物理快照（已存在拒绝覆盖）；`import [--dry-run]` 回灌：hash 幂等（已存在跳过不刷 ts/▶ 指针）、逐条重算 hash 完整性校验（损坏跳过警告）、单事务原子提交、保留原 ts/pinned、结束 enforce_max_items。格式为 Phase 5 插件化扩展点，选型见 ADR-004 | 备份可回灌（7 个单测锁定往返/幂等/ts/损坏拦截/裁剪/schema/快照 + 真机冒烟） |
| 2.5 | ✅ 大库长稳测试 | `crates/niri-clip-core/tests/large_db.rs`：100k 条规模下写入 / 查询 / 并发 / 维护 / 迁移五段自动化，每段自带条目数与内容断言；迁移段照 `migrate.rs` 的 v1/v2/v3 步骤**自建 v3 旧库**（含重复行与星标继承），覆盖外壳脚本做不到的那部分。默认 `#[ignore]`，规模可由 `NIRI_CLIP_STRESS_N` 覆盖。实测数值见 ARCHITECTURE §9 | 无锁死、无数据丢失、内存平稳 —— 三项均已断言并本机 100k 实测通过 |
| 2.6 | ✅ 工程收敛（v0.6 前置） | 全项目体检：结构冗余 / 文档失真 / 标准性三类共 38 项（含执行中新发现的 2 个 P0 代码缺陷），**零遗留**收尾——对外契约断链修复（AUR 服务单元路径、`cargo publish` 依赖 version）、删除确认三套实现收敛为 core 单一 15s TTL 状态机、`tui` 模块移出 core 恢复自定分层、依赖计数口径修正、文档失效机制写入 AGENTS.md | 38 项逐项有落地结果（修复 / 删除 / 明确驳回三者之一）；文档与代码零冲突；`docs/` 内无指向已删文档的引用；同语义只剩一处实现（`confirm` / `single_instance` / `render_row` / `preview_multiline` / `upsert_clip`）；门禁 fmt + clippy(`-D warnings`) + 71 测试全过 |
| 2.7 | 列表排序走索引（2.5 暴露） | `list()` 的排序首键是表达式 `(hash = ?2) DESC`（把 ▶ 当前项顶到第 1 行），SQLite 据此无法按索引有序扫描——100500 行库 `EXPLAIN QUERY PLAN` 实测退化为 `SCAN clips + USE TEMP B-TREE FOR ORDER BY`，10k→100k 耗时 0.95ms→108ms（114×，远超数据量增长的 10×）。修法方向：当前项单独取（`WHERE hash = ?2`）后与"其余按 pinned/ts 排序取 N 条"拼接，两条查询都能走索引。**默认 `max_items = 750` 下库不可能这么大，真实用户不触发**，故不阻塞 v0.6.0，可随后续 minor 交付 | 100k 下 `list(300)` 回到与 10k 同阶（不再随规模超线性增长）；实测数值记入 ARCHITECTURE §9 |

**技术要点：** tokenizer 选型已由 ADR-002 定为 **trigram**（原"unicode61 起步"计划已推翻——它对中文子串不可用）；迁移脚本幂等可回滚。
**时间节点：** 里程碑 **M2 = v0.6.0**（含 2.5 长稳与 2.6 工程收敛，一并收尾）。
**依赖：** 1.6 的基准设施（防止 FTS 引入性能回归无感知）；2.6 的契约修复为 4.2 AUR 三包齐备的前置。

---

## 六、Phase 3 — v0.7：安全与隐私强化

**核心目标：** 剪贴板是敏感数据重灾区，此阶段把"默认安全"做实，并验证加密可行性。

**关键任务：**

| # | 任务 | 要点 | 验收标准 |
|---|---|---|---|
| 3.1 | `ignore_regex` 强化 | 默认规则扩展（1Password/Bitwarden/OTP URI/KeePassXC 格式）；命中不落盘不通知 | 单元测试覆盖主流密码管理器输出格式 |
| 3.2 | 粘贴后通知脱敏 | 通知内容截断/打码 | 通知不泄露明文 |
| 3.3 | 敏感条目快速清除 | `wipe --sensitive`；TUI 内 `Ctrl-D` 快速删除当前 | 审计：密码类条目留存时长可人为清零 |
| 3.4 | 加密存储 PoC（调研） | 评估 age/sqlite encryption extension 的取舍，产出 ADR 文档；可行则出实验 flag | PoC 结论文档化，决定 v2.0 是否落地 |
| 3.5 | 安全审计自查 | 文件权限、日志脱敏、seccomp/systemd 沙箱加固（systemd user 单元加 ProtectSystem 等指令） | `systemd-analyze security` 评分改善；检查项清单归档 |

**技术要点：** age 加密对 SQLite 的透明层代价高，优先评估按条目加密（敏感类单独加密表）；systemd 沙箱指令零成本优先落地。
**时间节点：** 约 2–3 周；里程碑 **M3 = v0.7.0 安全版本**。

---

## 七、Phase 4 — v1.0：Production 正式发布

**核心目标：** 面向公众的 GA 版本——分发渠道全通、图片支持完整、文档与 CI 达到可维护开源项目标准。

**关键任务：**

| # | 任务 | 要点 | 验收标准 |
|---|---|---|---|
| 4.1 | 图片剪贴板完整支持 | 覆盖 image/png、jpeg、webp 全链路（捕获→存储→预览→重粘贴）；`wl-copy < image` 场景 | 手动 + 自动测试全过 |
| 4.2 | AUR 三包齐备 | `niri-clip`（release）、`niri-clip-git`、`niri-clip-bin`（预编译）。**决策（2026-08-28）：AUR 首次发布随 epic（原生 UI）合并后的首个版本一起**；主包构建 `-p niri-clip`（不含 gui 依赖链，makedepends 不需要 libxkbcommon），`niri-clip-gui` 待 M5.4 单独分包；`sqlite` 非运行依赖（rusqlite bundled）已移除 | 三包 CI 自动 bump + 安装冒烟 |
| 4.3 | crates.io 发布 | `cargo publish`，检查 package metadata 完整 | `cargo install niri-clip` 可用 |
| 4.4 | waybar 模块 | JSON 输出（历史条数/最新条目/daemon 状态），`niri-clip waybar` | waybar wiki 配置示例可用 |
| 4.5 | 文档完善 | man page 终稿、README 截图/GIF、ARCHITECTURE 同步、CHANGELOG 规范化（Keep a Changelog） | 外部用户仅凭文档可完成安装到使用 |
| 4.6 | CI 深化 | 增加：release 自动打包（GitHub Releases + SHA256）、AUR 机器人、benchmark 趋势 | tag 即全渠道发布，人工只做审核 |
| 4.7 | 兼容性矩阵验证 | fzf 版本门控（<0.71 回退 fuzzel）、主流终端（foot/alacritty/kitty）、niri 最新 stable | 矩阵清单归档并勾验 |
| 4.8 | 版本语义承诺 | 发布 v1.0.0，宣布 SemVer + 配置格式向后兼容承诺 | CHANGELOG 声明 |
| 4.9 | 运维体检 `doctor` | 新增 `niri-clip doctor`：自检 systemd 单元/flock 状态/niri spawn-at-startup 与 binds/依赖（fzf≥0.71/chafa）配置；升级文档写明 daemon 与 niri 两处 spawn 的取舍（只留一处，防遗漏） | 全新环境按 doctor 输出 5 分钟完成接入 |

**时间节点：** 约 3–4 周；里程碑 **M4 = v1.0.0 GA**（项目正式交付点）。

---

## 八、Phase 5 — v1.x：生态与集成（v1.0 后持续）

按需求热度滚动排期，每项独立可裁剪：

| 候选项 | 说明 | 前置 |
|---|---|---|
| **原生 UI 后续（核心已交付，收尾项见下）** | 消除终端冷启动瓶颈的彻底解。**已交付**：M5.1 选型 ADR-001（含修订 1：layer-shell → 常规 xdg 窗口 + tiny-skia 软渲染）→ M5.2 MVP → M5.3 语义对齐 → M5.4.1 后端选择接入，`crates/niri-clip-gui` 随 v0.5.x 发布。**剩余收尾**：① 窗口启动延迟真机实测（目标 ≤50ms，记入开销预算表）② 兼容矩阵（niri stable / sway）③ 随 minor 的文档与 CHANGELOG 补齐。原立项详案 `docs/NATIVE-UI.md` 已于本轮删除（内容已全部收敛至此表与 ADR-001） | 无（`niri-clip-core` 下沉早于 Phase 1 完成） |
| OSC52 远程剪贴板 | SSH/终端场景同步历史 | 1.1（selection 抽象） |
| niri overview 集成 | 预览窗口嵌入 niri 概览 | layer-shell 协议调研（随原生 UI 立项一并推进） |
| 历史内容动作插件化 | 自定义 action（URL 直接打开等） | 2.4 导出格式稳定 |
| foot server 模式 | `foot --server` 常驻 + `footclient` ~10ms 拉窗（终端方案的极限优化，原生 UI 交付后自然退役） | 用户安装 foot |
| 跨合成器通用化 | 抽离 niri 特定假设，支持 sway/hyprland | 无破坏性改动审计 |

节奏：每 6–8 周一个 minor（v1.1 / v1.2 ...），patch 随 bug 修复随时发。

---

## 九、Phase 6 — v2.0+：长期愿景

- **加密历史（age）**：基于 3.4 PoC 结论落地，默认关闭、opt-in
- **原生 GUI 前端**：iced/gtk4-layer-shell 二选一（以 PoC 定）
- **多设备同步**（远期探索）：加密导出 + 文件同步方案优先于自建网络服务

> 原则：v2.0 不预设时间表，由 v1.x 使用反馈驱动立项。

---

## 十、资源需求

**人力：** 单人维护者（当前模式）；v1.0 后若社区增长，需：issue 分诊志愿者、AUR co-maintainer、翻译贡献者（man/README 中英双语是低成本高收益项，可列入 Phase 4 可选项）。

**基础设施（均零成本）：**
- GitHub Actions（CI/release/AUR 机器人）
- AUR 账号 ×3 包；crates.io 账号
- 测试环境：本机 niri + 至少一个非 kitty 终端（foot）验证回退路径；可选 QEMU Wayland 虚拟机做干净环境冒烟

**外部依赖跟踪（每阶段开始时核查一次）：**
- fzf ≥0.71（`--id-nth` 依赖）、wl-clipboard-rs 0.9.x、rusqlite 0.31（bundled SQLite 含 FTS5）、chafa
- 关注点：wl-protocols 变更（ext-data-control-v1）、niri 配置语法变更

---

## 十一、可追踪性机制

1. **版本 ↔ 里程碑映射**：GitHub Milestones 与本文档 Phase 一一对应；每个任务建 issue，label 标注 `phase/1` … 与任务编号（如 `P1-3 图片配额 GC`）
2. **DoD 门禁**：任何任务关闭前必须——代码合并主分支、`cargo clippy -D warnings` 零警告、测试覆盖核心路径、CHANGELOG 有条目
3. **CI 门禁**（已有基础上递增）：fmt / clippy / test / release build / 冒烟（v0.4.1 已有）→ + benchmark 阈值（P1）→ + release 打包与 AUR bump（P4）；另有 **100k 大库长稳（2.5）走手动触发**的 `stress` job（`workflow_dispatch`，不入 PR 必过检查）
4. **CHANGELOG.md**：每个 tag 必须有对应章节，文档即发布记录
5. **本文档即单一真相源**：完成一项勾一项，Phase 状态标记（进行中/已交付）随 tag 更新

---

## 十二、动态调整机制

**评审节奏：**
- 每 Phase 结束：对照验收标准复盘一次，产出下一 Phase 的任务细化（当前仅细化到下一阶段，更远阶段保持粗粒度）
- 每两周（与 minor 节奏对齐）：核查外部依赖更新与 issue 热度，重排 Phase 5 候选项

**调整规则：**
1. **范围裁剪优先于延期**：Phase 内任务按 P0（阻塞里程碑）/P1（本阶段该做）/P2（可顺延）分级，超期时先砍 P2
2. **线上事故 > 一切排期**：复现 v0.4.1 级别（捕获停摆）的事故时，中断当前阶段修复（v0.4.1 的响应模式沿用）
3. **技术选型变更门槛**：仅当出现"定型技术无法达成的硬需求"时启动 ADR 评审，避免无谓重写
4. **粗粒度远期、细粒度近期**：Phase 5/6 只维护候选项清单，进入 Phase 前一阶段结束时再细化，防止远期规划腐化

---

## 十三、风险登记

| 风险 | 等级 | 应对 |
|---|---|---|
| fzf 上游破坏 `--track --id-nth` 行为 | 高 | 版本门控 + 兼容矩阵测试（4.7）；fuzzel 回退路径保持可用 |
| FTS5 中文搜索效果不佳 | 低（已闭环） | 已由 ADR-002 定 **trigram**（中英文子串均命中，实测 0.16ms）；若未来需按词命中，换 tokenizer 须走 `user_version` 新迁移重建索引 |
| wl-clipboard-rs API 变更/停维护 | 中 | 锁定 Cargo.lock；事件驱动主路径已不依赖其轮询 |
| blake3 全表重算迁移出错致数据翻倍/丢失 | 中 | 迁移事务内合并 + 条目数只减不增断言（2.2）；100k 长稳测试（2.5）；迁移前 VACUUM INTO 快照 |
| 依赖蔓生拖慢编译、增大二进制 | 中 | 新增依赖过开销审计（1.8 ✅，基线见 ARCHITECTURE §9）；编译时间/依赖数进开销预算表跟踪（预算已重估为 <120s，当前达标） |
| 单人 bus factor | 中 | 文档与 CI 即"第二维护者"；关键流程（发版/迁移）全部脚本化 |
| 加密方案性能不达标 | 低 | PoC 先行（3.4），不达标则 v2.0 降级为可选导出加密 |

---

## 十四、分支与协作规范

> 目的：main 分支只接受经过 CI 验证的变更，防止直推；单人维护同样执行
> （规范即未来协作者的 onboarding 文档）。

**1. main 保护（GitHub 设置，一次性配置）**
- Settings → Branches → Add branch protection rule（`main`）：
  - ✅ Require a pull request before merging（单人场景可设 "require 0 approvals"）
  - ✅ Require status checks：勾选 CI 全部六道工序（fmt/clippy/test/build/smoke/bench）
  - ✅ Do not allow force pushes / Do not allow deletions
- 未配置前的人工红线：`git push origin main` 禁止；只推功能分支

**2. 分支命名与 issue 关联**
- 格式：`<type>/<issue>-<slug>`，type ∈ feat / fix / perf / docs / chore / epic
- 例：`feat/12-native-ui-mvp`、`fix/15-reload-flash`、`epic/native-ui`
- ROADMAP 任务编号（P1-2、5.2.1）即 issue 标题前缀，分支必须挂 issue

**3. PR 规则**
- 一个 PR 聚焦一个任务；>400 行建议拆分
- 长期 epic（如原生 UI）用 **stacked PR**：core 下沉 → spike → MVP → 语义对齐，
  每个 PR 独立可运行、CI 全绿、可演示（附截图/asciinema）
- 合并方式：**squash merge**，保持 main 线性历史；commit 标题用 Conventional Commits

**4. commit message（Conventional Commits）**
- 格式：`<type>(<scope>): <what>`，正文回答"为什么"，ADR/事故关联放正文
- 例：`perf(tui): 启动提速 + 关闭闪窗修复`（正文含诊断结论与取舍）

**5. 紧急修复 fast-track**
- 线上事故（如 v0.4.1 捕获停摆级）走 `hotfix/<issue>-<slug>` 分支 + 最小 PR，
  仍需 CI 绿才可合并；合并后立即 tag patch 版本

**6. 本地门禁（提交前自检）**
- `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test --locked`
- 行为变更必须：CHANGELOG 条目 + 相关文档同步 + 测试覆盖
