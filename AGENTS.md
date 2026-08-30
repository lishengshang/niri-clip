# AGENTS.md — AI 协作开发约定

> 本文件约束一切 AI 助手在本仓库内的行为。**AI 是执行者，用户是唯一的决策者。**
> 代码可以完全自主编写与验证，但一切触碰 Git 写面与发布链的动作必须先经用户明确批准。
> 分支/PR/commit 的完整规范见 `docs/ROADMAP.md` §14，本文件是其上的 AI 强约束层，当然如果开发者或者用户的路线不确定或者模糊也可以给出建议。

## 一、红线（默认禁止，除非用户当轮明确指示）

### Git 写操作

1. **禁止擅自 commit**：不执行 `git commit`（含 `--amend`）。用户说"提交/commit"才允许，且提交前必须先展示 `git status` + `git diff` 摘要与拟用的提交信息，等用户确认或按其修改意见调整。
2. **禁止擅自 push**：不执行 `git push`（任何分支、任何形式）。`--force` / `--force-with-lease` 一律禁止——即使被要求，对 main 也必须先警告。
3. **禁止创建 PR**：不使用 `gh pr create`，不主动建议合并/关闭他人的 PR，可以最后给用户意见
4. **禁止破坏性操作**：`reset --hard`、`rebase`、`checkout -- .` / `restore`、`clean`、`stash drop/clear`、`branch -D`、`tag -d`、`cherry-pick` 到 main——除非用户明确点名该操作并确认范围。
5. **禁止改写仓库配置**：`git config`（本地/全局）、hooks、`.gitattributes`、`.github/` 下任何文件（CI/模板/工作流）——CI 变更影响所有贡献者，必须单独立项经用户批准。
6. **禁止乱写 issue/里程碑**：不创建、编辑、关闭 issue，不加 label/milestone/assignee。issue 由用户（或经用户批准的脚本）管理。
7. 一切都和用户商量和建议，规范的流程

### 发布链文件（改动即影响分发，默认只读）

- `Cargo.toml` 的 `version`、`PKGBUILD*`、`.SRCINFO*`、`assets/niri-clip.service`、`.github/workflows/`
- 版本号 bump、tag、release 只能由用户决策；AI 可在"建议动作清单"里提议，不得直接执行。
- 如有错误和必要的更改以及问题需更改和用户建议和请求

### 范围与隐私

- 只改任务相关文件；顺手重构、格式化无关文件、批量重命名一律不做，如出现确实需要更改和用户交流
- `git add` 只加明确相关的文件，**禁止** **`git add -A`** **/** **`git add .`**（防误提 `.env`、state、截图、二进制）。
- 不把用户真实数据（剪贴板内容、DB、state 目录、截图）写进仓库或测试。

## 二、默认工作流（每轮任务）

```
理解任务 → 读代码（先读后改） → 最小改动实现
→ 本地门禁：cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
→ 汇报：改了什么/为什么/验证结果 + 【建议的 Git 动作清单】
→ 停下，等用户决策
```

- 改动保持在**工作区**，留给用户 `git diff` 审查；不主动 stash/commit 来"清理现场"。
- 行为变更必须同步：CHANGELOG 条目 + 相关文档（ROADMAP 勾选/ADR）+ 测试覆盖，缺一在汇报中说明。
- 遇到与本文件冲突的用户指令：执行用户指令，但先复述风险。

## 三、每轮收尾：【建议的 Git 动作清单】（固定格式）

AI 每轮结束时必须给出以下清单，**只建议、不执行**，由用户逐项决策：

```markdown
## 建议 Git 动作（待确认）
1. commit：<拟用 Conventional Commits 信息>（含文件列表，N 个文件 / +x -y）
2. CHANGELOG：已更新/建议条目：<...>
3. push：建议推送到 <分支>（当前规范：禁止直推 main，应推 <fix/xx-yy> 后开 PR）
4. 版本/发布：是否需要 bump / tag：<建议>
5. 其他：issue/文档/CI 相关建议（如有）
—— 请逐项确认或修改，确认前我不会执行任何一项。
```

## 四、提交规范（用户批准 commit 后执行）

- 信息用 Conventional Commits：`<type>(<scope>): <what>`，正文回答"为什么"；中文正文、诊断结论与取舍入正文。
- 一次 commit 一个主题；不相关改动拆分提交。
- 用 heredoc 传递多行信息，不加 `--no-verify`。
- 提交后 `git status` 复核并回报 hash。

## 五、其他约束

- **部署/系统级动作**（安装二进制、重启 systemd、改 niri 配置、写用户 dotfiles）同属"写面"：仅当用户明确要求"部署/安装"时执行，执行前说明将触碰的路径。
- 长任务先给 TODO 计划再动手；计划变更随时同步。
- 验证优先于推断：能跑测试/复现的结论不以"静态分析认为"收尾。

