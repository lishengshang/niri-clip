//! 验证无控制终端时的选择器路由。用临时 XDG 目录和假 PATH，避免启动桌面程序。

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Sandbox {
    root: PathBuf,
    cli: PathBuf,
}

impl Sandbox {
    fn new(tag: &str, backend: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("niri-clip-tui-{}-{}", tag, std::process::id()));
        for dir in ["bin", "old-bin", "config/niri-clip", "home"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(
            root.join("config/niri-clip/config.toml"),
            format!("tui_backend = \"{backend}\"\nnotify_enabled = false\n"),
        )
        .unwrap();
        let cli = root.join("niri-clip");
        // 避免把上一次构建留在目标目录的 GUI 误当作本测试的后端。
        let binary = env!("CARGO_BIN_EXE_niri-clip");
        if fs::hard_link(binary, &cli).is_err() {
            fs::copy(binary, &cli).unwrap();
        }
        let sandbox = Self { root, cli };
        sandbox.write_exe("bin/foot", r#"printf 'foot\n' >> "$TRACE""#);
        sandbox.write_exe(
            "bin/fuzzel",
            r#"printf 'fuzzel\n' >> "$TRACE"
/bin/cat >/dev/null"#,
        );
        sandbox
    }

    fn write_exe(&self, path: &str, source: &str) {
        let file = self.root.join(path);
        fs::write(&file, format!("#!/bin/sh\n{source}\n")).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn add_fzf(&self, path: &str, version: &str) {
        self.write_exe(
            path,
            &format!(
                r#"printf '%s %s\n' '{version}' "$1" >> "$TRACE"
printf '%s\n' '{version}'"#
            ),
        );
    }

    fn command(&self, argv: &[&str], path: &Path) -> Command {
        let setsid =
            which::which("setsid").expect("setsid is needed to remove the controlling TTY");
        let mut command = Command::new(setsid);
        command
            .arg("--wait")
            .arg(&self.cli)
            .args(argv)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", path)
            .env("TRACE", self.root.join("trace"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state with ' quote"))
            .env("XDG_CACHE_HOME", self.root.join("cache"));
        command
    }

    fn open(&self, path: &Path) {
        let output = self.command(&["tui"], path).output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn seed_history(&self) {
        // 使用合成条目，并检查实际打开 fuzzel 的路径而非空库提前返回。
        let mut child = self
            .command(&["store"], &self.root.join("bin"))
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"synthetic").unwrap();
        assert!(child.wait().unwrap().success());
    }

    fn trace(&self) -> String {
        fs::read_to_string(self.root.join("trace")).unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn fuzzel_bypasses_terminal_wrapping() {
    let sandbox = Sandbox::new("explicit-fuzzel", "fuzzel");
    sandbox.seed_history();
    sandbox.open(&sandbox.root.join("bin"));
    assert_eq!(sandbox.trace(), "fuzzel\n");
}

#[test]
fn native_fallback_rechecks_the_selected_fzf() {
    let sandbox = Sandbox::new("native-fallback", "native");
    sandbox.seed_history();
    sandbox.add_fzf("bin/fzf", "0.74.0");
    sandbox.add_fzf("old-bin/fzf", "0.70.0");
    let bin = sandbox.root.join("bin");
    sandbox.open(&bin);
    assert!(sandbox
        .root
        .join("state with ' quote/niri-clip/tui.log")
        .exists());
    let path = std::env::join_paths([sandbox.root.join("old-bin"), bin]).unwrap();
    sandbox.open(Path::new(&path));
    assert_eq!(
        sandbox.trace(),
        "0.74.0 --version\nfoot\n0.70.0 --version\nfuzzel\n"
    );
}
