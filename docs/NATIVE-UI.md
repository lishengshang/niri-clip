# 原生 UI 立项详案（Phase 5 正式任务 · 2026-08-28 立项）

> 状态：**已立项，未排期**——执行窗口默认在 v1.0 GA 之后（不阻塞 Production 交付）；
> 若优先级调整需同步修订 ROADMAP 总览表。
> 立项动线：Mod+V 链路优化中发现终端冷启动（ghostty ~150ms+）是 TUI 方案的结构性
> 瓶颈，fzf 依赖 TTY 无法绕开；原生 layer-shell 窗口是唯一彻底解。

---

## 1. 问题与目标

**问题**：TUI = fzf 承载在终端模拟器里。每次 Mod+V 都是一次终端冷启动
（Wayland 连接/字体/着色器/窗口表面），这是链路里压不掉的大头；
fuzzel 兜底虽是原生 layer-shell 但无 reload/track，功能残缺。

**目标（验收口径）**：

| 指标 | 目标 |
|---|---|
| Mod+V 到窗口可交互 | ≤50ms（当前 ~200ms+） |
| 交互语义 | 与 fzf 版 100% 对齐：▶ 当前项置顶、★、1-9 快选、Ctrl-Y 连续复制、星标删除确认、搜索过滤、预览 |
| 内存 | 按需形态退出归零；若选常驻形态，常驻增量 ≤10MB（开销预算表跟踪） |
| 依赖 | 新依赖闭包过开销审计（产品原则 2），TUI 用户可不装 GUI 依赖 |
| 回退 | `tui_backend` 三态可用：native / fzf / fuzzel；native 缺依赖时自动降级 |

**非目标**：不做鼠标拖拽/多窗口/设置界面；预览首版可用进程外 chafa。

---

## 2. 技术选型 PoC（M5.1 → 产出 ADR-001）

| 候选 | 优势 | 风险 |
|---|---|---|
| **iced + iced_layershell** | 纯 Rust、声明式、Elm 架构贴合现有代码风格 | layershell crate 成熟度/上游联动待验证 |
| **gtk4-layer-shell（Rust 绑定）** | layer-shell 生态最成熟、IME/渲染交给 GTK | 依赖闭包重（GTK 全家桶），需 workspace 拆分隔离 |
| **smithay-client-toolkit 手写** | 最轻、依赖最少、完全可控 | 工作量最大，文本渲染/IME 全自建 |

**评估维度**（spike 实测，不许拍脑袋）：启动延迟 / 常驻内存 / 依赖闭包 crate 数 /
二进制增量 / 键盘导航与中文渲染 / **搜索框 IME（zwp_text_input）支持** / 图片预览路径 / 上游活跃度。

**PoC 交付物**：同一 spike 场景（300 条列表 + 键盘导航 + Enter 复制 + 搜索框）
在两个候选下各跑一遍，数据记入 ROADMAP 开销预算表，ADR-001 定选型 + 进程形态。

---

## 3. 进程形态 PoC（并入 ADR-001）

| 形态 | 说明 | 取舍 |
|---|---|---|
| **A 按需进程** `niri-clip gui` | 每次 Mod+V 起新进程创建 layer surface | 启动 ~10-30ms；内存退出归零；实现简单——**倾向此项**（简洁原则） |
| **B 常驻 UI 服务** | daemon 内嵌 UI 线程，socket 激活显示 | 显示路径零启动；常驻内存 +；状态同步复杂 |

---

## 4. 代码组织（前置重构）

Cargo workspace 拆分，保证"选 fzf 的用户不为 GUI 依赖买单"：

```
crates/
  niri-clip-core/   # store/config/daemon 逻辑下沉（纯 lib，无 UI）
  niri-clip/        # CLI + TUI（现 src/main.rs 路径）
  niri-clip-gui/    # 原生 UI（选型后建，独立可选打包）
```

- 前置任务 **5.0 core 下沉**：纯移动 + pub 导出调整，行为零变化，CI 全绿，独立 PR
- 打包策略（主包含不含 GUI / 分包 niri-clip-gui）在 M5.4 按依赖体积数据定

---

## 5. 里程碑与任务分解

### M5.1 选型 PoC + ADR-001（~1–2 周）
- [ ] 5.0 core 下沉为 lib crate（前置，独立 PR）
- [ ] 5.1.1 iced_layershell spike（含 IME 验证）
- [ ] 5.1.2 gtk4-layer-shell spike
- [ ] 5.1.3 实测数据表 + ADR-001：定框架与进程形态

### M5.2 MVP（~2–3 周）
- [ ] 5.2.1 窗口创建 + 列表渲染（`menu_clips` 数据、num/▶/★ 三列语义）
- [ ] 5.2.2 键盘导航 + Enter 复制（wl-copy 路径复用，写 current 指针）+ Esc 关闭
- [ ] 5.2.3 `tui_backend = "native"` 配置接入 + niri binds 文档

### M5.3 语义对齐（~2–3 周）
- [ ] 5.3.1 增量搜索过滤（fzf fuzzy 等价物，中文可用）
- [ ] 5.3.2 1-9 快选 + Ctrl-Y 不退出连续复制（▶ 跟随语义同步）
- [ ] 5.3.3 pin/删除 + 星标删除内嵌确认（对齐 1.5 交互）
- [ ] 5.3.4 预览：文本复用 `preview_text`；图片按 ADR 结论（chafa 进程 or 纹理）

### M5.4 打磨发布（~1–2 周）
- [ ] 5.4.1 后端选择逻辑：auto → native 可用则用之，缺依赖降级 fzf/fuzzel
- [ ] 5.4.2 窗口启动延迟纳入 criterion 基准与开销预算表
- [ ] 5.4.3 兼容矩阵（niri stable / sway）、文档、CHANGELOG、随 minor 发布

**总量 ~6–10 周**（单人每周 8–12h）。每个任务一个 issue + 独立分支，
M5.2 起每个 PR 可运行可演示（分支规范见 ROADMAP 十四）。

---

## 6. 风险登记

| 风险 | 等级 | 应对 |
|---|---|---|
| iced_layershell 成熟度/上游破坏 | 高 | spike 先行；GTK 候选兜底；锁版本 |
| 搜索框 IME 不支持（zwp_text_input） | 中 | 列入 spike 必测项；不支持则该候选直接出局 |
| 依赖闭包膨胀违背低开销 | 中 | workspace 拆分 + 分包；依赖数进开销预算表 |
| 双 UI 长期维护成本 | 中 | 语义全部收敛在 store/core 层，UI 只做渲染层 |
| fzf 与 native 行为漂移 | 中 | manual.sh 语义断言双后端各跑一遍 |

---

## 7. 与总路线的关系

- 执行窗口：**v1.0 GA 之后**（Phase 5 主体）；5.0 core 下沉如需提前可独立执行（纯重构，无风险）
- 交付版本：MVP 随 v1.1，语义对齐后 v1.2 宣布 native 为 auto 默认（fzf 降为可选）
- fuzzel 兜底路径永久保留（无 GUI 依赖环境的最小可用形态）
