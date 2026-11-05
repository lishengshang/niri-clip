//! `niri-clip doctor` 端到端测试：假 PATH（systemctl/fzf/wl-clipboard）+ XDG 沙盒。
//!
//! 成功用例锁定“环境可接入”时退出码为 0；失败用例锁定配置错误会以非零
//! 退出码暴露给安装脚本/CI。

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_niri-clip")
}

#[cfg(unix)]
fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).expect("write fake executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake executable");
}

fn sandbox(tag: &str, extra_config: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("niri-clip-doctor-{}-{}", tag, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("config/niri-clip")).unwrap();
    fs::create_dir_all(dir.join("config/niri")).unwrap();
    fs::create_dir_all(dir.join("config/systemd/user")).unwrap();
    fs::create_dir_all(dir.join("state/niri-clip")).unwrap();
    fs::create_dir_all(dir.join("cache")).unwrap();
    fs::create_dir_all(dir.join("home")).unwrap();
    fs::create_dir_all(dir.join("fakebin")).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            dir.join("state/niri-clip"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }

    fs::write(
        dir.join("config/niri-clip/config.toml"),
        format!(
            "notify_enabled = false\nenable_image_preview = false\ntui_backend = \"fzf\"\n{extra_config}"
        ),
    )
    .unwrap();
    fs::write(
        dir.join("config/niri/config.kdl"),
        "include \"binds.kdl\"\nspawn-at-startup \"niri-clip\" \"daemon\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("config/niri/binds.kdl"),
        "binds {\n    Mod+V { spawn \"niri-clip\" \"tui\"; }\n}\n",
    )
    .unwrap();
    fs::write(
        dir.join("config/systemd/user/niri-clip.service"),
        "[Service]\nStateDirectory=niri-clip\nRestrictAddressFamilies=AF_UNIX\n",
    )
    .unwrap();

    write_exe(
        &dir.join("fakebin/systemctl"),
        "#!/bin/sh\ncase \"$*\" in\n  *is-active*) echo inactive ;;\n  *is-enabled*) echo disabled ;;\n  *--version*) echo 'systemd 257 (257.8-1-test)' ;;\nesac\nexit 0\n",
    );
    write_exe(
        &dir.join("fakebin/fzf"),
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '0.74.0 (test)'; fi\nexit 0\n",
    );
    for bin in ["wl-paste", "wl-copy", "timeout"] {
        write_exe(&dir.join(format!("fakebin/{bin}")), "#!/bin/sh\nexit 0\n");
    }

    dir
}

fn doctor(dir: &Path) -> Output {
    Command::new(exe())
        .arg("doctor")
        .env("HOME", dir.join("home"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env("NIRI_CONFIG", dir.join("config/niri/config.kdl"))
        .env("WAYLAND_DISPLAY", "wayland-test")
        .env("SQLITE_TMPDIR", dir)
        .env(
            "PATH",
            format!("{}:/usr/bin:/bin", dir.join("fakebin").display()),
        )
        .output()
        .expect("run niri-clip doctor")
}

fn cleanup(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn healthy_environment_exits_zero_with_actionable_report() {
    let dir = sandbox("ok", "");
    let out = doctor(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        out.status.success(),
        "健康环境应以 0 退出；stdout={stdout} stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("niri-clip doctor"), "缺少标题：{stdout}");
    assert!(
        stdout.contains("已展开 1 个"),
        "应展开 include 中的 binds.kdl：{stdout}"
    );
    assert!(
        stdout.contains("[ok] 快捷键：Mod+V → niri-clip tui"),
        "应识别 include 文件中的 Mod+V 绑定：{stdout}"
    );
    assert!(stdout.contains("fzf 0.74.0"), "应识别 fzf 版本：{stdout}");
    assert!(stdout.contains("0 失败"), "健康环境不应有失败项：{stdout}");

    cleanup(&dir);
}

#[test]
fn broken_config_fails_with_nonzero_exit() {
    let dir = sandbox("bad", "max_items = 0\n");
    let out = doctor(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(!out.status.success(), "配置错误必须以非零退出");
    assert!(stdout.contains("[fail] max_items"), "缺少失败项：{stdout}");
    assert!(
        stderr.contains("doctor 发现"),
        "stderr 应给出失败摘要：{stderr}"
    );

    cleanup(&dir);
}
