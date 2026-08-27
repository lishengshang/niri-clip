use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::time::Duration;
use tokio::time::sleep;

use wl_clipboard_rs::paste::{self, get_contents, ClipboardType, MimeType, Seat};

use crate::config::Config;
use crate::store;

/// `niri-clip store` : 从 stdin 读取剪贴板内容并入库
pub fn store_from_stdin() -> Result<()> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    if buf.trim().is_empty() {
        // 尝试从 wl-paste 获取（当 --watch 未管道时）
        if let Ok((mut pipe, _)) =
            get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text)
        {
            let mut v = Vec::new();
            if pipe.read_to_end(&mut v).is_ok() && !v.is_empty() {
                buf = String::from_utf8_lossy(&v).to_string();
            }
        }
    }
    if buf.trim().is_empty() {
        return Ok(());
    }
    // v0.4：入库失败不再静默——此前调用方多以 `let _ =` 吞错，排障困难
    let inserted = store::insert(buf, None)?;
    if inserted {
        eprintln!("[niri-clip store] inserted");
    } else {
        eprintln!("[niri-clip store] deduplicated/ignored");
    }
    // v1.0 独立软件：不再双写 cliphist，迁移请用 niri-clip migrate 一次性导入
    Ok(())
}

/// 原生 Wayland 轮询 daemon（自 v0.3）。
///
/// 已知取舍（记录于 ARCHITECTURE §2）：500ms 轮询存在 <500ms 连续复制的
/// 丢帧窗口与空闲往返开销；事件驱动（data-control SelectionChanged 监听）
/// 为 v0.5 规划项，届时轮询降级为 `polling_fallback=true` 的兜底配置。
async fn run_native() -> Result<()> {
    println!("[niri-clip daemon] native wl-clipboard-rs polling (500ms)");
    let mut last_hash: Option<String> = None;
    loop {
        // 尝试获取文本
        let text_opt = match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text)
        {
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
                    paste::Error::ClipboardEmpty
                    | paste::Error::NoSeats
                    | paste::Error::NoMimeType => {
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
                // v0.4：与 store 入库 hash 同源复用，避免两处指纹实现漂移
                let key = store::hash_text(trimmed);
                if last_hash.as_ref() != Some(&key) {
                    last_hash = Some(key);
                    if let Err(e) = store::insert(trimmed.to_string(), None) {
                        eprintln!("[daemon native] store error: {e:#}");
                    }
                }
            }
        } else if Config::load().enable_image_preview {
            // 尝试图片 mime
            for mime in ["image/png", "image/jpeg", "image/webp"] {
                if let Ok((mut pipe, _)) = get_contents(
                    ClipboardType::Regular,
                    Seat::Unspecified,
                    MimeType::Specific(mime),
                ) {
                    let mut v = Vec::new();
                    if pipe.read_to_end(&mut v).is_ok() && !v.is_empty() {
                        let key = store::image_content_key(mime, &v);
                        if last_hash.as_ref() != Some(&key) {
                            last_hash = Some(key.clone());
                            // 内容 key 已含稳定 FNV 指纹 + 字节长度：
                            // 修复旧版仅按 len 判重导致等长异图丢失的问题；
                            // 数据文件按 clip id 落盘并由 image_path 精确关联
                            match store::insert_image(mime, &v) {
                                Ok(Some(img)) => {
                                    eprintln!("[daemon native] stored image #{}", img.id);
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    eprintln!("[daemon native] insert image error: {e:#}")
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }

        sleep(Duration::from_millis(500)).await;
    }
}

/// 单实例锁。v0.4 新增：双开 daemon 会产生双路轮询、通知重复，并在库上
/// 造成无谓的并发写入。flock 优点是进程崩溃即自动释放，无陈锁残留问题。
fn acquire_single_instance() -> Result<std::fs::File> {
    let dir = Config::state_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("daemon.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("open lock {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: 对自身打开的 fd 执行 flock；语义由内核保证
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(anyhow!(
                "另一个 niri-clip daemon 正在运行（锁 {}）",
                path.display()
            ));
        }
    }
    Ok(file)
}

/// 单次探测即可判定原生通道可用性。
///
/// v0.4 修复 panic 隐患：旧实现连续调用两次 get_contents 且对第二次
/// `unwrap_err()`——若两次之间剪贴板恰好变为可用则直接 panic，
/// 在 systemd Restart=on-failure 下表现为周期性崩启。
fn probe_native_available() -> bool {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
        Ok(_) => true,
        Err(paste::Error::ClipboardEmpty | paste::Error::NoMimeType | paste::Error::NoSeats) => {
            true
        }
        Err(e) => {
            eprintln!("[niri-clip daemon] native probe failed: {:?}", e);
            false
        }
    }
}

/// Daemon 入口：优先原生轮询，失败回退 wl-paste --watch
pub async fn run() -> Result<()> {
    Config::ensure_dirs()?;
    let _lock_file = acquire_single_instance()?;
    let cfg = Config::load();
    println!(
        "[niri-clip daemon] max_items={} tui={} image_preview={}",
        cfg.max_items, cfg.tui_backend, cfg.enable_image_preview
    );
    println!("[niri-clip daemon] db: {}", Config::db_path().display());

    for bin in ["wl-paste", "wl-copy"] {
        if which::which(bin).is_err() {
            eprintln!("[warn] missing {bin}");
        }
    }

    let enable = dirs::config_dir().map(|d| d.join("niri/clipboard-history.enabled"));
    if enable.as_deref().map(|p| p.exists()).unwrap_or(false) {
        eprintln!("[niri-clip] 检测到旧的 clipboard-history.enabled，建议迁移: niri-clip migrate");
    }

    println!("[niri-clip daemon] trying native wl-clipboard-rs...");
    let native_ok = probe_native_available();

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
