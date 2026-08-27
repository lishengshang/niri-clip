use anyhow::Result;
use std::io::Read;
use std::time::Duration;
use tokio::time::sleep;

use crate::config::Config;
use crate::store;

/// `niri-clip store` : 从 stdin 读取剪贴板内容并入库
pub fn store_from_stdin() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        // 尝试从 wl-paste 获取（当 --watch 未管道时）
        if let Ok((mut pipe, _)) = wl_clipboard_rs::paste::get_contents(
            wl_clipboard_rs::paste::ClipboardType::Regular,
            wl_clipboard_rs::paste::Seat::Unspecified,
            wl_clipboard_rs::paste::MimeType::Text,
        ) {
            let mut v = Vec::new();
            if pipe.read_to_end(&mut v).is_ok() && !v.is_empty() {
                buf = String::from_utf8_lossy(&v).to_string();
            }
        }
    }
    if buf.trim().is_empty() {
        return Ok(());
    }
    let inserted = store::insert(buf, None)?;
    if inserted {
        eprintln!("[niri-clip store] inserted");
    } else {
        eprintln!("[niri-clip store] deduplicated/ignored");
    }
    // v1.0 独立软件：不再双写 cliphist，迁移请用 niri-clip migrate 一次性导入
    Ok(())
}

/// 原生 Wayland 轮询 daemon（v0.3）
async fn run_native() -> Result<()> {
    println!("[niri-clip daemon] native wl-clipboard-rs polling (500ms)");
    let mut last_hash: Option<String> = None;
    loop {
        // 尝试获取文本
        let text_opt = match wl_clipboard_rs::paste::get_contents(
            wl_clipboard_rs::paste::ClipboardType::Regular,
            wl_clipboard_rs::paste::Seat::Unspecified,
            wl_clipboard_rs::paste::MimeType::Text,
        ) {
            Ok((mut pipe, _)) => {
                let mut v = Vec::new();
                if pipe.read_to_end(&mut v).is_ok() {
                    // 尝试 utf8
                    String::from_utf8(v).ok()
                } else {
                    None
                }
            }
            Err(e) => {
                match e {
                    wl_clipboard_rs::paste::Error::ClipboardEmpty
                    | wl_clipboard_rs::paste::Error::NoSeats
                    | wl_clipboard_rs::paste::Error::NoMimeType => {
                        // 空剪贴板，正常
                    }
                    _ => eprintln!("[daemon native] paste error: {:?}", e),
                }
                None
            }
        };

        if let Some(text) = text_opt {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let hash = format!("{}-{}", trimmed.len(), {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut h = DefaultHasher::new();
                    trimmed.hash(&mut h);
                    h.finish()
                });
                if last_hash.as_ref() != Some(&hash) {
                    last_hash = Some(hash);
                    let _ = store::insert(trimmed.to_string(), None);
                }
            }
        } else {
            // 尝试图片 mime
            if Config::load().enable_image_preview {
                for mime in ["image/png", "image/jpeg", "image/webp"] {
                    if let Ok((mut pipe, _)) = wl_clipboard_rs::paste::get_contents(
                        wl_clipboard_rs::paste::ClipboardType::Regular,
                        wl_clipboard_rs::paste::Seat::Unspecified,
                        wl_clipboard_rs::paste::MimeType::Specific(mime),
                    ) {
                        let mut v = Vec::new();
                        if pipe.read_to_end(&mut v).is_ok() && !v.is_empty() {
                            let hash = format!("img-{}-{}", mime, v.len());
                            if last_hash.as_ref() != Some(&hash) {
                                last_hash = Some(hash);
                                // 存占位文本 + mime
                                let placeholder = format!("[image {} {} bytes]", mime, v.len());
                                // 同时缓存二进制到文件供 chafa 预览
                                let cache = dirs::cache_dir().unwrap().join("niri-clip/images");
                                let _ = std::fs::create_dir_all(&cache);
                                let path = cache.join(format!("{}.bin", chrono::Utc::now().timestamp_millis()));
                                let _ = std::fs::write(&path, &v);
                                let _ = store::insert(placeholder, Some(mime.to_string()));
                            }
                            break;
                        }
                    }
                }
            }
        }

        sleep(Duration::from_millis(500)).await;
    }
}

/// Daemon 入口：优先原生，失败回退 wl-paste --watch
pub async fn run() -> Result<()> {
    Config::ensure_dirs()?;
    let cfg = Config::load();
    println!("[niri-clip daemon] max_items={} tui={} image_preview={}", cfg.max_items, cfg.tui_backend, cfg.enable_image_preview);
    println!("[niri-clip daemon] db: {:?}", Config::db_path());

    for bin in ["wl-paste", "cliphist"] {
        if which::which(bin).is_err() {
            eprintln!("[warn] missing {}", bin);
        }
    }

    let enable = dirs::config_dir().unwrap().join("niri/clipboard-history.enabled");
    if enable.exists() {
        eprintln!("[niri-clip] 检测到旧的 clipboard-history.enabled，建议迁移: niri-clip migrate");
    }

    // 尝试原生
    println!("[niri-clip daemon] trying native wl-clipboard-rs...");
    // 快速探测 native 是否可用
    let native_ok = wl_clipboard_rs::paste::get_contents(
        wl_clipboard_rs::paste::ClipboardType::Regular,
        wl_clipboard_rs::paste::Seat::Unspecified,
        wl_clipboard_rs::paste::MimeType::Text,
    )
    .is_ok()
        || matches!(
            wl_clipboard_rs::paste::get_contents(
                wl_clipboard_rs::paste::ClipboardType::Regular,
                wl_clipboard_rs::paste::Seat::Unspecified,
                wl_clipboard_rs::paste::MimeType::Text,
            )
            .unwrap_err(),
            wl_clipboard_rs::paste::Error::ClipboardEmpty
                | wl_clipboard_rs::paste::Error::NoMimeType
                | wl_clipboard_rs::paste::Error::NoSeats
        );

    if native_ok {
        println!("[niri-clip daemon] native available, using polling");
        let _ = notify_rust::Notification::new()
            .summary("niri-clip")
            .body("守护进程已启动 (native)")
            .show();
        return run_native().await;
    }

    // 回退 fork
    println!("[niri-clip daemon] native not available, fallback to wl-paste --watch");
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let mut child = tokio::process::Command::new("wl-paste")
        .arg("--watch")
        .arg(&exe)
        .arg("store")
        .spawn()?;
    println!("[niri-clip daemon] watching via wl-paste --watch ...");
    let _ = notify_rust::Notification::new()
        .summary("niri-clip")
        .body("守护进程已启动 (wl-paste)")
        .show();
    let status = child.wait().await?;
    eprintln!("[niri-clip daemon] wl-paste exited: {:?}", status);
    Ok(())
}
