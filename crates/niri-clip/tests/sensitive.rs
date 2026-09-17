//! 任务 3.3 的端到端回归：`wipe --sensitive` 与 `delete-current`（真实二进制，
//! XDG 三变量沙盒全隔离）。
//!
//! 沙盒配置固定 `ignore_regex = 'TOPSECRET_[0-9]+'`——自定义规则整体替换
//! 默认值（3.1 语义），保证种子文本不会被捕获路径过滤，命中判定只认该前缀。
//! 通知与图片预览关闭，CI（无显示服务）与真机（不偷读真实剪贴板）均确定性。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_niri-clip")
}

/// 三变量全隔离沙盒（只设部分 XDG 变量会读写真实用户配置/库，见交接板 §3）
fn sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "niri-clip-sensitive-{}-{}",
        tag,
        std::process::id()
    ));
    fs::create_dir_all(dir.join("config/niri-clip")).expect("create config dir");
    fs::create_dir_all(dir.join("state")).expect("create state dir");
    fs::create_dir_all(dir.join("cache")).expect("create cache dir");
    fs::write(
        dir.join("config/niri-clip/config.toml"),
        "notify_enabled = false\nenable_image_preview = false\nignore_regex = 'NOMATCH_XQ'\n",
    )
    .expect("write sandbox config");
    dir
}

fn clip_cmd(dir: &PathBuf) -> Command {
    let mut cmd = Command::new(exe());
    cmd.env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("SQLITE_TMPDIR", dir);
    cmd
}

/// 经 `store` stdin 入库（走真实捕获路径：过滤/指针/去重与生产一致）
fn store_text(dir: &PathBuf, text: &str) {
    let mut child = clip_cmd(dir)
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
        .write_all(text.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("store");
    assert!(out.status.success(), "store failed: {:?}", out.stderr);
}

fn run(dir: &PathBuf, args: &[&str]) -> std::process::Output {
    let out = clip_cmd(dir).args(args).output().expect("run command");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn list_raw(dir: &PathBuf) -> Vec<String> {
    let out = run(dir, &["list-raw"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

/// 改写沙盒 ignore_regex（模拟"规则在入库之后才存在/被强化"——命中规则的
/// 文本进不了库，敏感残留只可能来自规则缺失期的存量）
fn set_regex(dir: &Path, regex: &str) {
    fs::write(
        dir.join("config/niri-clip/config.toml"),
        format!("notify_enabled = false\nenable_image_preview = false\nignore_regex = '{regex}'\n"),
    )
    .expect("rewrite sandbox config");
}

/// `wipe --sensitive`：dry-run 只统计；真删命中条目（含星标）、保留普通条目
#[test]
fn wipe_sensitive_dry_run_then_purge() {
    let dir = sandbox("wipe");
    set_regex(&dir, "NOMATCH_XQ");
    store_text(&dir, "TOPSECRET_1");
    store_text(&dir, "normal entry");
    store_text(&dir, "TOPSECRET_2");
    set_regex(&dir, "TOPSECRET_[0-9]+");

    let dry = run(&dir, &["wipe", "--sensitive", "--dry-run"]);
    let dry_out = String::from_utf8_lossy(&dry.stdout);
    assert!(dry_out.contains("2"), "dry-run 应报 2 条：{dry_out}");
    assert_eq!(list_raw(&dir).len(), 3, "dry-run 不得真删");

    // 星标一条敏感条目：敏感清除不保护星标（留存时长须可清零）
    let listing = list_raw(&dir);
    let id = listing
        .iter()
        .find(|l| l.contains("TOPSECRET_1"))
        .and_then(|l| l.split('\t').nth(3))
        .expect("list-raw 行第 4 列为 id")
        .to_string();
    run(&dir, &["pin", &id]);

    run(&dir, &["wipe", "--sensitive"]);
    let rest = list_raw(&dir);
    assert_eq!(rest.len(), 1, "应只剩普通条目");
    assert!(rest[0].contains("normal entry"), "实际：{rest:?}");
    cleanup(&dir);
}

/// `delete-current`：一把删掉 ▶ 当前项（最后捕获者）；指针失效后再跑仍成功
#[test]
fn delete_current_removes_last_capture_without_selection() {
    let dir = sandbox("delcur");
    store_text(&dir, "hello-current");
    store_text(&dir, "second");
    // 最后捕获者在第 1 行（当前项）
    assert!(list_raw(&dir)[0].contains("second"));

    run(&dir, &["delete-current"]);
    let rest = list_raw(&dir);
    assert_eq!(rest.len(), 1);
    assert!(rest[0].contains("hello-current"));

    // 再按一次（fzf 场景会 reload 后继续可按）：指针已失效，必须成功且零删
    run(&dir, &["delete-current"]);
    assert_eq!(list_raw(&dir).len(), 1);
    cleanup(&dir);
}

/// `delete-current` 对星标当前项遵循 ADR-005：首按挂起不删，同 id 二按才删
#[test]
fn delete_current_respects_pinned_double_confirm() {
    let dir = sandbox("delcur-star");
    store_text(&dir, "starred-current");
    let id = list_raw(&dir)[0]
        .split('\t')
        .nth(3)
        .expect("id 列")
        .to_string();
    run(&dir, &["pin", &id]);

    let out = run(&dir, &["delete-current"]);
    // fzf 语义（--fzf）挂起时静默；此处走 CLI 文案路径
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("pending"), "星标当前项首按应挂起：{stdout}");
    assert_eq!(list_raw(&dir).len(), 1, "挂起阶段不得真删");

    run(&dir, &["delete-current"]);
    assert!(list_raw(&dir).is_empty(), "二按应真删");
    cleanup(&dir);
}
