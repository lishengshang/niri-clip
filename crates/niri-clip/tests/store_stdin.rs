//! `niri-clip store` stdin 入库的回归测试（XDG 三变量沙盒全隔离）。
//!
//! 覆盖限流入口门的文本语义：> `max_clip_bytes` 的载荷在图片不可走时
//! 必须按文本超限拒绝、零入库；正常小文本不受影响。
//!
//! 沙盒配置固定 `enable_image_preview = false` 与 `notify_enabled = false`，
//! 由此超限分支不触碰 Wayland、不发桌面通知——CI（无显示服务）与真机
//! （不偷读用户真实剪贴板）都确定性成立。图片正路径（超文本限的截图
//! 经图片限额重新裁决入库）依赖真实剪贴板内容，不可移植模拟，需真机
//! 会话人工验证，不在自动化范围。

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_niri-clip")
}

/// 三变量全隔离沙盒 + 关闭图片捕获与通知的最小配置。
/// 只设部分 XDG 变量会读写到真实用户配置/库（见交接板 §3）。
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niri-clip-store-stdin-{}-{}",
        tag,
        std::process::id()
    ));
    fs::create_dir_all(dir.join("config/niri-clip")).expect("create config dir");
    fs::create_dir_all(dir.join("state")).expect("create state dir");
    fs::create_dir_all(dir.join("cache")).expect("create cache dir");
    fs::write(
        dir.join("config/niri-clip/config.toml"),
        "notify_enabled = false\nenable_image_preview = false\n",
    )
    .expect("write sandbox config");
    dir
}

fn store_cmd(dir: &PathBuf) -> Command {
    let mut cmd = Command::new(exe());
    cmd.arg("store")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        // SQLite 临时文件与库同盘，防"库在沙盒、临时文件在 /tmp"错配
        .env("SQLITE_TMPDIR", dir);
    cmd
}

fn list_raw(dir: &PathBuf) -> String {
    let out = Command::new(exe())
        .arg("list-raw")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("SQLITE_TMPDIR", dir)
        .output()
        .expect("run list-raw");
    assert!(out.status.success(), "list-raw failed: {:?}", out.stderr);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

/// 超过 `max_clip_bytes` 的二进制载荷在图片捕获关闭时必须按文本超限拒绝：
/// 退出码 0、stderr 报文本限额、零入库。锁定超限复查不得误入库/误报
/// 图片限额，也锁定入口门本身的拒绝语义不被后续改动吞掉。
#[test]
fn oversize_stdin_reports_text_limit_and_stores_nothing() {
    let dir = sandbox("oversize");

    let mut child = store_cmd(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn store");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        // 子进程按 take(max+1) 读完即退，剩余写入触发 EPIPE 属预期，
        // 与生产侧 wl-paste 收 SIGPIPE 的行为一致
        let _ = stdin.write_all(&vec![0xFFu8; 2 * 1024 * 1024]);
    }
    let out = child.wait_with_output().expect("wait store");

    assert!(
        out.status.success(),
        "store 必须以成功语义拒绝超限：{:?}",
        out.stderr
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("max_clip_bytes=1048576"),
        "stderr 应报文本限额，实际: {stderr}"
    );
    assert!(
        !stderr.contains("max_image_bytes"),
        "图片捕获关闭时不得报图片限额: {stderr}"
    );
    assert!(list_raw(&dir).trim().is_empty(), "超限载荷必须零入库");

    cleanup(&dir);
}

/// 正常小文本经 stdin 入库不受限流复查改动影响。
#[test]
fn text_stdin_still_captures_normally() {
    let dir = sandbox("text");

    let mut child = store_cmd(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn store");
    {
        let mut stdin = child.stdin.take().expect("child stdin");
        stdin
            .write_all(b"niri-clip-stdin-smoke-8f3a\n")
            .expect("write payload");
    }
    let out = child.wait_with_output().expect("wait store");
    assert!(out.status.success(), "store failed: {:?}", out.stderr);

    let rows = list_raw(&dir);
    assert!(
        rows.contains("niri-clip-stdin-smoke-8f3a"),
        "小文本必须正常入库，实际 list-raw 输出: {rows:?}"
    );

    cleanup(&dir);
}
