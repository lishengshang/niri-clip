//! 图片全链路端到端测试（任务 4.1）：捕获 → 存储 → 预览 → 重粘贴。
//!
//! 走法与生产一致：`wl-paste` 把图片原始字节从 stdin 直灌 `niri-clip store`
//! （管道不带 MIME，core 靠 magic 判定类型）；`niri-clip copy` 再经 `wl-copy`
//! 写回剪贴板——测试用假 `wl-copy` 截获 argv 与 stdin，断言 MIME 与字节完全
//! 一致。预检路径故意不给 PATH 放 `chafa`，锁定"没装 chafa 时给出可读提示"
//! 而不是静默失败。
//!
//! 沙盒：XDG 三变量全隔离 + 独立 state 目录，不触碰真实用户库与剪贴板；
//! PATH 只含 fakebin，故无需 Wayland 会话，CI 确定性成立。

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_niri-clip")
}

fn write_exe(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, body).expect("write fake executable");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod fake executable");
}

/// 假 wl-copy：把 argv 与 stdin 原样落盘，供断言 MIME 与字节完整性。
/// 用绝对路径 `/bin/cat`——沙盒 PATH 只含 fakebin（为了让 chafa 必然缺席），
/// 裸 `cat` 会因找不到命令而写出空文件，把断言失败伪装成"字节不一致"。
const FAKE_WL_COPY: &str = r#"#!/bin/sh
printf '%s\n' "$@" > "$FAKE_CAPTURE/argv"
/bin/cat > "$FAKE_CAPTURE/payload"
"#;

struct Sandbox {
    root: PathBuf,
    capture: PathBuf,
}

impl Sandbox {
    fn new(tag: &str, extra_config: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("niri-clip-image-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("config/niri-clip")).unwrap();
        fs::create_dir_all(root.join("state")).unwrap();
        fs::create_dir_all(root.join("cache")).unwrap();
        fs::create_dir_all(root.join("fakebin")).unwrap();
        let capture = root.join("capture");
        fs::create_dir_all(&capture).unwrap();

        fs::write(
            root.join("config/niri-clip/config.toml"),
            format!(
                "notify_enabled = false\nenable_image_preview = true\ntui_backend = \"fzf\"\n{extra_config}"
            ),
        )
        .unwrap();
        write_exe(&root.join("fakebin/wl-copy"), FAKE_WL_COPY);

        Self { root, capture }
    }

    /// 沙盒内运行二进制；PATH 只含 fakebin（无 chafa/wl-paste，结果可复现）。
    fn run(&self, args: &[&str], stdin: Option<&[u8]>) -> Output {
        let mut cmd = Command::new(exe());
        cmd.args(args)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("SQLITE_TMPDIR", &self.root)
            .env("FAKE_CAPTURE", &self.capture)
            .env("PATH", self.root.join("fakebin"))
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn niri-clip");
        {
            let mut pipe = child.stdin.take().expect("child stdin");
            if let Some(bytes) = stdin {
                let _ = pipe.write_all(bytes);
            }
        }
        child.wait_with_output().expect("wait niri-clip")
    }

    fn store(&self, payload: &[u8]) -> Output {
        self.run(&["store"], Some(payload))
    }

    fn list_raw(&self) -> String {
        let out = self.run(&["list-raw"], None);
        assert!(
            out.status.success(),
            "list-raw 失败：{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// 列表行是 tab 分隔的 序号 / ▶ / ★ / id / 预览，id 取第 4 列
    fn ids(&self) -> Vec<i64> {
        self.list_raw()
            .lines()
            .filter_map(|line| line.split('\t').nth(3))
            .filter_map(|id| id.parse().ok())
            .collect()
    }

    fn copied(&self) -> (Vec<String>, Vec<u8>) {
        let argv = fs::read_to_string(self.capture.join("argv"))
            .expect("假 wl-copy 未收到参数")
            .lines()
            .map(str::to_string)
            .collect();
        let payload = fs::read(self.capture.join("payload")).expect("假 wl-copy 未收到 stdin");
        (argv, payload)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// 构造带指定 magic 的图片载荷，尾部标记用于验证字节完整性
fn payload(magic: &[u8], marker: &[u8], len: usize) -> Vec<u8> {
    let mut v = magic.to_vec();
    v.extend_from_slice(b"niri-clip-4.1-");
    v.extend_from_slice(marker);
    v.resize(len.max(v.len()), 0x5A);
    v
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG: &[u8] = b"\xFF\xD8\xFF\xE0";
const WEBP: &[u8] = b"RIFF\x00\x00\x00\x00WEBP";

/// png/jpeg/webp 三条同构路径：stdin 入库 → 复制回剪贴板，MIME 与字节完全一致。
#[test]
fn all_three_image_types_roundtrip_through_store_and_copy() {
    for (tag, magic, mime) in [
        ("png", PNG, "image/png"),
        ("jpeg", JPEG, "image/jpeg"),
        ("webp", WEBP, "image/webp"),
    ] {
        let sb = Sandbox::new(tag, "");
        let bytes = payload(magic, tag.as_bytes(), 512);

        let out = sb.store(&bytes);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{tag} store 失败：{stderr}");
        assert!(
            stderr.contains("stored image #") && stderr.contains(mime),
            "{tag} 应经 magic 判定入库并报出类型，实际 stderr：{stderr}"
        );

        let rows = sb.list_raw();
        assert!(
            rows.contains(&format!("[image {mime} {} bytes]", bytes.len())),
            "{tag} 列表占位应带 MIME 与字节数，实际：{rows:?}"
        );

        let ids = sb.ids();
        assert_eq!(ids.len(), 1, "{tag} 应只有一条条目");
        let out = sb.run(&["copy", &ids[0].to_string()], None);
        assert!(
            out.status.success(),
            "{tag} copy 失败：{:?}",
            String::from_utf8_lossy(&out.stderr)
        );

        let (argv, copied) = sb.copied();
        assert!(
            argv.contains(&format!("--type={mime}")),
            "{tag} 复制必须带上原始 MIME，实际 argv：{argv:?}"
        );
        assert_eq!(copied, bytes, "{tag} 重粘贴字节必须与入库时一致");
    }
}

/// 截图常态：> max_clip_bytes（默认 1 MiB）但 < max_image_bytes（10 MiB）。
/// 锁定向图片限额续读的逻辑——按文本限额截断会存下残缺图片。
#[test]
fn screenshot_above_text_limit_is_stored_whole() {
    let sb = Sandbox::new("big", "");
    let bytes = payload(PNG, b"big", 2 * 1024 * 1024);

    let out = sb.store(&bytes);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "大图 store 失败：{stderr}");
    assert!(
        !stderr.contains("max_clip_bytes"),
        "截图超过文本限额不能被当成文本超限拒绝：{stderr}"
    );

    let ids = sb.ids();
    assert_eq!(ids.len(), 1, "大图应入库：{}", sb.list_raw());

    let out = sb.run(&["copy", &ids[0].to_string()], None);
    assert!(out.status.success(), "copy 失败：{:?}", out.stderr);
    let (_, copied) = sb.copied();
    assert_eq!(copied.len(), bytes.len(), "入库字节不应被截断");
    assert_eq!(copied, bytes, "大图重粘贴字节必须完整");
}

/// 超过 max_image_bytes 的图片：明确报图片限额、零入库、退出码 0
/// （与文本超限一致：拒绝是正常语义，不是错误）。
#[test]
fn image_above_max_image_bytes_is_rejected() {
    let sb = Sandbox::new("oversize", "max_image_bytes = 64\n");
    let bytes = payload(PNG, b"too-big", 512);

    let out = sb.store(&bytes);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "超限拒绝不应是非零退出：{stderr}");
    assert!(
        stderr.contains("max_image_bytes=64"),
        "应报图片限额而非文本限额：{stderr}"
    );
    assert!(
        !stderr.contains("max_clip_bytes"),
        "不得误报文本限额：{stderr}"
    );
    assert!(sb.ids().is_empty(), "超限图片必须零入库：{}", sb.list_raw());
}

/// 同一张图重复复制只留一条（`img:` FNV 指纹去重），且不刷新出重复行。
#[test]
fn duplicate_image_is_deduplicated() {
    let sb = Sandbox::new("dedupe", "");
    let bytes = payload(PNG, b"dup", 256);

    assert!(sb.store(&bytes).status.success());
    assert!(sb.store(&bytes).status.success());

    assert_eq!(sb.ids().len(), 1, "重复图片应只保留一条：{}", sb.list_raw());
}

/// 没装 chafa 时预览给出可操作提示（不是静默空白、不 panic）。
#[test]
fn preview_without_chafa_explains_the_gap() {
    let sb = Sandbox::new("preview", "");
    assert!(sb.store(&payload(PNG, b"preview", 128)).status.success());
    let id = sb.ids()[0];

    let out = sb.run(&["preview", &id.to_string()], None);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "preview 失败：{:?}", out.stderr);
    assert!(
        stdout.contains("预览需要安装 chafa"),
        "缺少 chafa 时应提示安装，实际：{stdout:?}"
    );
}

/// 关闭图片捕获时，管道里的图片字节不得入库（开关必须真的能关掉）。
#[test]
fn image_capture_disabled_stores_nothing() {
    let sb = Sandbox::new("disabled", "enable_image_preview = false\n");
    let _ = sb.store(&payload(PNG, b"off", 256));
    assert!(
        sb.ids().is_empty(),
        "enable_image_preview=false 时不得入库：{}",
        sb.list_raw()
    );
}
