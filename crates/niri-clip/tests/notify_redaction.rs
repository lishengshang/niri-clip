//! 任务 3.2 的不变式守卫：**通知永不携带条目明文**（真实二进制端到端）。
//!
//! 原理：`notify::send` 经 PATH 调 `notify-send`。测试预置一个假
//! `notify-send`（把收到的参数追加到 `$NIRI_CLIP_NOTIFY_LOG`），随后：
//! 1. 捕获超限载荷（唯一会发通知的捕获分支）→ 通知必须只有限额数字、
//!    不含载荷内容；
//! 2. 正常捕获 → 不产生任何通知（明文无从泄露）。
//!
//! 这把"全部调用点均无明文"从人工审计结论变成回归锁定的结构不变式：
//! 将来任何调用点若开始格式化条目内容，本测试即红。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_niri-clip")
}

fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niri-clip-notify-{}-{}", tag, std::process::id()));
    fs::create_dir_all(dir.join("config/niri-clip")).expect("create config dir");
    fs::create_dir_all(dir.join("state")).expect("create state dir");
    fs::create_dir_all(dir.join("cache")).expect("create cache dir");
    fs::create_dir_all(dir.join("fakebin")).expect("create fakebin");
    // 假 notify-send：参数原样落日志（经 timeout 调起，退出码 0）
    fs::write(
        dir.join("fakebin/notify-send"),
        "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> \"$NIRI_CLIP_NOTIFY_LOG\"; done\n",
    )
    .expect("write fake notify-send");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dir.join("fakebin/notify-send"),
            fs::Permissions::from_mode(0o755),
        )
        .expect("chmod fake notify-send");
    }
    // notify_enabled = true：让通知路径真实走通；ignore_regex 自定义以隔离
    // 默认关键词（本测试不依赖过滤语义）
    fs::write(
        dir.join("config/niri-clip/config.toml"),
        "notify_enabled = true\nenable_image_preview = false\nmax_clip_bytes = 64\nignore_regex = 'NOMATCH_XQ'\n",
    )
    .expect("write sandbox config");
    dir
}

fn clip_cmd(dir: &Path, log: &Path) -> Command {
    let mut cmd = Command::new(exe());
    cmd.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("SQLITE_TMPDIR", dir)
        .env("NIRI_CLIP_NOTIFY_LOG", log);
    // 假 notify-send 置于 PATH 首位；coreutils timeout 仍从系统 PATH 命中
    cmd.env(
        "PATH",
        format!("{}:{}", dir.join("fakebin").display(), "/usr/bin:/bin"),
    );
    cmd
}

fn store_stdin(dir: &Path, log: &Path, payload: &str) {
    let mut child = clip_cmd(dir, log)
        .arg("store")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn store");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(payload.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("store");
    assert!(out.status.success(), "store failed: {:?}", out.stderr);
}

/// 轮询日志最多 3s（通知走后台线程，父进程退出先后不确定）；
/// 超时返回 None = 期间未出现任何通知
fn poll_log(log: &Path, deadline: Duration) -> Option<String> {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if let Ok(s) = fs::read_to_string(log) {
            if !s.is_empty() {
                return Some(s);
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

/// 唯一会发通知的捕获分支（超限拒绝）只报限额数字，不回显载荷内容
#[test]
fn oversize_notification_carries_no_payload_plaintext() {
    let dir = sandbox("oversize");
    let log = dir.join("notify.log");
    // 载荷含独特标记且 > max_clip_bytes(64)
    let payload = format!("NIRI-CLIP-{}-{}", "PAYLOAD-MARKER", "x".repeat(200));
    store_stdin(&dir, &log, &payload);

    let logged = poll_log(&log, Duration::from_secs(3)).expect("超限捕获必须发出通知");
    assert!(logged.contains("max_clip_bytes"), "通知应报限额：{logged}");
    assert!(
        !logged.contains("PAYLOAD-MARKER"),
        "通知不得携带载荷明文：{logged}"
    );
    cleanup(&dir);
}

/// 正常捕获不发任何通知：明文无从进入通知通道
#[test]
fn normal_capture_sends_no_notification_at_all() {
    let dir = sandbox("normal");
    let log = dir.join("notify.log");
    store_stdin(&dir, &log, "hello-notify-redaction");
    assert!(
        poll_log(&log, Duration::from_millis(700)).is_none(),
        "正常捕获不得产生通知（日志应为空）"
    );
    cleanup(&dir);
}
