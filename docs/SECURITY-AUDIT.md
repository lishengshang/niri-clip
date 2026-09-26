# 安全审计检查项清单（Phase 3 / 任务 3.5）

> **临时工作文档**：3.5 交付物归档本体。清单项全部闭环（含强化档决策）后，
> 按文档退役纪律由用户决定去留；有效结论（如沙箱指令取舍）随落地同步进
> ARCHITECTURE / ADR，不在本文档重复留存。
> 审计日期：2026-09-26。审计范围：v0.7 前夕 main（`5b413cd`）。

## ① 文件权限核查

原则：库/导出/图片等承载条目明文的文件 0600，承载目录 0700。统一入口
`store::tighten_dir_perms` / `tighten_file_perms`（`store/mod.rs`），调用点全覆盖：

| 对象 | 期望 | 实现点 | 结论 |
|---|---|---|---|
| `state/`（db 父目录） | 0700 | `connect()`（store/mod.rs:47）；migrate.rs:27/286 | ✅ |
| `state/db.sqlite`（含 -wal/-shm） | 0600 | `connect()`（store/mod.rs:57）；WAL/SHM 由 SQLite 保证与主库同权限 | ✅ |
| `state/images/` | 0700 | 主插入（store/mod.rs:275）+ import（backup.rs:311） | ✅ |
| `state/images/{id}.bin` | 0600 | `.tmp-` 先写再原子 rename 后收紧：主插入（store/mod.rs:295）+ import（backup.rs:316） | ✅ |
| export NDJSON（`--sqlite` 快照同） | 0600 | backup.rs:98/195（父目录 0700：94/189）；注意 `File::create` 默认 0644，收紧紧随其后 | ✅ |
| blake3 迁移快照 | 0600 | migrate.rs:292 | ✅ |
| `state/{name}.lock`（flock） | 0644（默认） | `OpenOptions` 无 mode（single_instance.rs:32） | ⚠️ 观察项 O1 |
| `state/pending_delete` | 0644（默认） | `fs::write`（confirm.rs:68） | ⚠️ 观察项 O2 |

**观察项处置**：O1/O2 文件内容均为非敏感（flock 文件空、pending_delete 仅
`<id> <毫秒时间戳>`），且父目录 0700 已阻断其他用户访问，**不构成缺陷**。
daemon 路径由 §③ `UMask=0077` 一并收为 0600；非 daemon 的一次性 CLI 路径
保持现状（同用户会话内无更高权限读者）。

## ② 日志脱敏审计

口径：与 3.2 通知不变式一致——日志/通知只允许状态文案、数量、尺寸、路径、
条目 ID，不得携带条目明文。全仓库 55 处 `println!`/`eprintln!`（含 GUI，
不含测试）逐条归类：

- **状态/计数/配置数字**（daemon 启动横幅、清理计数、超限提示）✅
- **路径**（db 路径、图片路径、迁移源/目标）✅ 路径非条目明文
- **条目 ID / 下标**（`copied {id}`、GUI `[dbg] sel=/sid=`）✅ ID 允许
- **错误链 `{e:#}`**（io/sqlite 错误，含路径，不含载荷）✅
- **功能输出**（`print` 子命令的 `println!("{}", c.text)`，tui.rs:533）✅
  用户主动查看条目是该命令的目的，非日志泄露
- ⚠️ **观察项 O3**：`store` 命令非 UTF-8 载荷分支把载荷头部 64 字节 lossy
  转后前 32 字符 debug 打印到 stderr（daemon.rs:118）。该分支只承接二进制
  （文本走 UTF-8 分支），泄的是图片字节头（PNG magic 等）而非文本明文；
  stderr 进 user journal 仅用户自身/root 可读。**低风险，不修**；若未来
  收紧，改打 `{len} bytes` 即可

**结论：全部调用点零条目明文**，与 3.2 的 `tests/notify_redaction.rs`
端到端守卫互为印证。新增调用点回归口径：先过 3.2 的通知守卫思路再进日志。

## ③ systemd user 单元沙箱加固

**基线**（加固前，可复现）：

```
systemd-analyze security --offline=true assets/niri-clip.service   # systemd ≥253
→ Overall exposure level: 9.4 UNSAFE
```

主要扣分：CapabilityBoundingSet 全空（0.5）、PrivateNetwork（0.5）、
SystemCallFilter 全空（~1.5）、PrivateUsers/PrivateTmp（0.4）、IPAddressDeny
（0.2）、UMask（0.1，"服务新建文件默认全局可读"）等。

**核心档指令**（已落地 `assets/niri-clip.service`，零功能风险，`niri-clip
daemon` 只需 AF_UNIX + 读写自身 state）：

```ini
UMask=0077
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=read-only
StateDirectory=niri-clip
CapabilityBoundingSet=
RestrictAddressFamilies=AF_UNIX
IPAddressDeny=any
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
ProtectClock=true
ProtectHostname=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectKernelLogs=true
ProtectControlGroups=true
SystemCallFilter=@system-service
SystemCallFilter=~@resources
SystemCallErrorNumber=EPERM
```

**实测结果（2026-09-26）**：`9.4 UNSAFE → 1.7 OK`。剩余扣分构成该服务
的结构性地板，不再追求：AF_UNIX（wayland 必需）、ProtectHome read-only
（state 在 home 下）、PrivateNetwork/PrivateUsers（强化档）、ProtectProc/
ProcSubset/MemoryDenyWriteExecute（强化档）、DeviceAllow char-rtc:r
（PrivateDevices 默认白名单，只读时钟）、User=/RootDirectory（`--offline`
按 system 上下文口径误判，user manager 实机不计）。可选再拿分项（收益
≤0.4，未采用）：`SystemCallArchitectures=native`、`SystemCallFilter=
~@privileged`（会把 @chown 一并禁掉，收益 0.2，真机验证后可加）。

关键取舍：

- **`StateDirectory=niri-clip`** 而非手写 `ReadWritePaths=`：user manager 下
  解析为 `$XDG_STATE_HOME/niri-clip`（默认 `~/.local/state/niri-clip`），
  **目录不存在时自动创建**并纳入读写白名单，规避 `ReadWritePaths=` 指向
  不存在路径导致单元启动失败的坑，且用户自定义 `XDG_STATE_HOME` 自动跟随。
  要求 systemd ≥248（user manager 支持 StateDirectory=）
- **`PrivateNetwork=true` 不采用**，以 `IPAddressDeny=any` 替代：daemon 与
  其子进程（wl-paste/wl-copy/notify-send）确无网络需求，但独立 netns 对
  子进程行为的隔离收益为零，deny-by-address 已拿到同等评分项且失败面更小
- **`PrivateUsers` 不采用**：wayland compositor 侧普遍用 SO_PEERCRED 校验
  client uid，映射 uid 可能被 niri 拒绝连接——归入强化档真机验证后再议

**强化档**（须真机逐项验证后再开，默认不开）：

```ini
MemoryDenyWriteExecute=true   # Rust 静态码无 JIT，理论兼容；真机验证
ProtectProc=invisible
PrivateUsers=true             # ⚠ wayland SO_PEERCRED uid 校验风险，见上
```

**验收**（ROADMAP 3.5）：

1. 离线评分：`systemd-analyze security --offline=true assets/niri-clip.service`
   ✅ 已达标：**9.4 → 1.7 OK**（目标 ≤3.0）
2. 真机（用户执行）：重装 service 后 `systemctl --user daemon-reload &&
   systemctl --user restart niri-clip`，确认捕获/粘贴/通知链路正常，再跑
   `systemd-analyze security niri-clip.service` 记录实机分
3. 回滚：指令块整体删除即恢复原状，无状态残留
