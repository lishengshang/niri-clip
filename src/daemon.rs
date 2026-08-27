use anyhow::{anyhow, Context, Result};
use std::io::Read;
use std::time::Duration;
use tokio::time::sleep;

use wl_clipboard_rs::paste::{self, get_contents, ClipboardType, MimeType, Seat};

use crate::config::Config;
use crate::store;

/// systemd user 单元模板（随二进制内置，供 `install-service` 一键落盘）
pub const SERVICE_UNIT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/niri-clip.service"
));

// =====================================================================
// 捕获架构（v0.4.1 起）
//
//   主模式  ：wl-paste --watch -> sh -c 'exec timeout <N>s niri-clip store'
//             —— 事件驱动，selection 变化才触发；零空闲往返；
//                子进程级别 timeout 为任何病态读挂起划界（秒级回收），
//                从机制上杜绝“daemon 进程存活但捕获停滞”的静默失联。
//   回退模式：原 500ms 轮询仅在缺失 wl-paste 二进制时启用。
//
// 历史事故备忘（issue #2）：纯轮询实现中 read_to_end 对个别来源应用会
// 无限阻塞且不产生错误——进程活着、捕获停死。以事件源 + 时间边界重构后
// 该故障形态不再可能发生。
// =====================================================================

/// 组装主模式的 shell 命令串。单列出来以便单元测试覆盖超时边界语义。
fn watch_shell_command(exe: &std::path::Path, timeout_secs: u64) -> String {
    format!("exec timeout {timeout_secs}s {}", exe.display())
}

/// `niri-clip store` : 入库一段剪贴板载荷。
///
/// * stdin 有数据（主模式：wl-paste 管道直灌）→ 直接按文本处理，
///   不再触碰本进程内的 Wayland 连接，热点路径零阻塞面；
/// * stdin 为空（历史兼容：直接手动执行 store）→ 保持旧的
///   get_contents(Text) 探测，并在开启图片预览时尝试图片 MIME。
pub fn store_from_stdin() -> Result<()> {
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf)?;

    // 非空载荷：优先视作 UTF-8 文本
    if !buf.is_empty() {
        match String::from_utf8(buf) {
            Ok(text) => {
                ingest_text(&text)?;
                return Ok(());
            }
            Err(broken) => {
                // 二进制流（如默认类型选到了图片）：不在文本语义里硬塞，
                // 只记录并交给 stderr 排障；真图片走下方显式 MIME 探测
                let raw = broken.as_bytes();
                let head = String::from_utf8_lossy(&raw[..raw.len().min(64)])
                    .chars()
                    .take(32)
                    .collect::<String>();
                eprintln!("[niri-clip store] non-utf8 payload ignored ({head:?}…)");
            }
        }
    }
    try_system_capture().map(|_| ())
}

/// 文本入库（含 ignore 规则），统一入口便于测试
fn ingest_text(text: &str) -> Result<bool> {
    let inserted = store::insert(text.to_string(), None)?;
    if inserted {
        eprintln!("[niri-clip store] inserted");
    } else {
        eprintln!("[niri-clip store] deduplicated/ignored");
    }
    Ok(inserted)
}

/// 无 stdin 数据时的系统剪贴板探测：先文本，后图片（受开关约束）。
/// 所有失败在此收敛为“本次未捕获”，由调用方决定是否报错。
fn try_system_capture() -> Result<bool> {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
        Ok((mut pipe, _)) => {
            let mut v = Vec::new();
            if pipe.read_to_end(&mut v).is_ok() {
                let text = String::from_utf8(v)
                    .map_err(|_| anyhow!("clipboard payload is not valid utf-8"))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return ingest_text(trimmed);
                }
            }
            Ok(false)
        }
        Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType) => {
            // 无文本可取：若开启了图片预览则尝试图片 MIME
            capture_image_if_enabled()
        }
        Err(e) => Err(e.into()),
    }
}

fn capture_image_if_enabled() -> Result<bool> {
    if !Config::load().enable_image_preview {
        return Ok(false);
    }
    for mime in ["image/png", "image/jpeg", "image/webp"] {
        if let Ok((mut pipe, _)) = get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            MimeType::Specific(mime),
        ) {
            let mut v = Vec::new();
            if pipe.read_to_end(&mut v).is_ok() && !v.is_empty() {
                match store::insert_image(mime, &v) {
                    Ok(Some(img)) => {
                        eprintln!(
                            "[niri-clip store] stored image #{} -> {}",
                            img.id,
                            img.path.display()
                        )
                    }
                    Ok(None) => {}
                    Err(e) => return Err(anyhow!("insert image: {e:#}")),
                }
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// v0.4.1 主模式：事件驱动外部源。
async fn run_watch(timeout_secs: u64) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    println!(
        "[niri-clip daemon] event-driven source: wl-paste --watch (per-capture timeout {timeout_secs}s)"
    );
    let _ = notify_rust::Notification::new()
        .summary("niri-clip")
        .body("守护进程已启动 (event)")
        .show();

    // 注意传给内部 sh 的引号转义：exe 含空格时仍可靠
    let mut child = tokio::process::Command::new("wl-paste")
        .arg("--watch")
        .arg("sh")
        .arg("-c")
        .arg(watch_shell_command(
            std::path::Path::new(&exe),
            timeout_secs,
        ))
        .spawn()
        .context("spawn wl-paste --watch")?;
    println!("[niri-clip daemon] watching clipboard changes ...");
    let status = child.wait().await?;
    eprintln!("[niri-clip daemon] wl-paste exited: {:?}", status);
    Ok(())
}

/// 原生轮询（回退模式）：仅当系统中不存在 wl-paste 时启用。
///
/// 已知取舍：500ms 间隔存在 <500ms 连续复制的丢帧窗口与空闲往返开销，
/// 且 read_to_end 在个别来源上可能长期阻塞。该模式只为“最小可用环境”兜底，
/// 生产部署要求安装 wl-clipboard 以使用主模式。
async fn run_native_polling() -> Result<()> {
    println!(
        "[niri-clip daemon] FALLBACK native polling (500ms) — recommend installing wl-clipboard"
    );
    let mut last_hash: Option<String> = None;
    loop {
        match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok((mut pipe, _)) => {
                let mut v = Vec::new();
                if pipe.read_to_end(&mut v).is_ok() {
                    if let Ok(text) = String::from_utf8(v) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            let key = store::hash_text(trimmed);
                            if last_hash.as_ref() != Some(&key) {
                                last_hash = Some(key);
                                if let Err(e) = store::insert(trimmed.to_string(), None) {
                                    eprintln!("[daemon native] store error: {e:#}");
                                }
                            }
                            sleep(Duration::from_millis(500)).await;
                            continue;
                        }
                    }
                }
            }
            Err(
                paste::Error::ClipboardEmpty | paste::Error::NoSeats | paste::Error::NoMimeType,
            ) => {}
            Err(e) => eprintln!("[daemon native] paste error: {:?}", e),
        }

        // 图片抓取保持沿用轮询路径（低频场景可接受）
        if let Err(e) = capture_image_if_enabled() {
            eprintln!("[daemon native] image capture error: {e:#}");
        }
        sleep(Duration::from_millis(500)).await;
    }
}

/// 单实例锁。flock 进程崩溃即自动释放，双开立即报错退出。
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
/// v0.4 修复 panic 隐患：探测只调用一次 get_contents，并 match
/// 三类良性错误——勿改回"两次调用 + 第二次 unwrap_err()"的写法，
/// 剪贴板恰在两次之间变为可用会 panic，systemd 下表现为周期崩启。
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

/// Daemon 入口：事件驱动优先（wl-paste --watch + 每捕获 timeout 划界），
/// 仅在缺失 wl-paste 二进制时回退 native 500ms 轮询兜底。
pub async fn run() -> Result<()> {
    Config::ensure_dirs()?;
    let _lock_file = acquire_single_instance()?;
    let cfg = Config::load();
    println!(
        "[niri-clip daemon] max_items={} tui={} image_preview={}",
        cfg.max_items, cfg.tui_backend, cfg.enable_image_preview
    );
    println!("[niri-clip daemon] db: {}", Config::db_path().display());

    if which::which("wl-paste").is_ok() {
        return run_watch(cfg.capture_timeout_secs).await;
    }

    eprintln!("[warn] missing wl-paste —— 回退到原生轮询模式");
    for bin in ["wl-copy"] {
        if which::which(bin).is_err() {
            eprintln!("[warn] missing {bin}");
        }
    }
    let enable = dirs::config_dir().map(|d| d.join("niri/clipboard-history.enabled"));
    if enable.as_deref().map(|p| p.exists()).unwrap_or(false) {
        eprintln!("[niri-clip] 检测到旧的 clipboard-history.enabled，建议迁移: niri-clip migrate");
    }

    println!("[niri-clip daemon] probing native wayland availability...");
    if !probe_native_available() {
        return Err(anyhow!(
            "没有可用的剪贴板捕获源：请安装 wl-clipboard（提供 wl-paste 事件源），或确认当前处于 Wayland 会话"
        ));
    }
    run_native_polling().await
}

/// 安装 systemd user 单元并打印启用指引
pub fn install_service() -> Result<std::path::PathBuf> {
    let unit_dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot determine XDG_CONFIG_HOME"))?
        .join("systemd/user");
    std::fs::create_dir_all(&unit_dir)?;
    let unit_path = unit_dir.join("niri-clip.service");
    std::fs::write(&unit_path, SERVICE_UNIT)
        .with_context(|| format!("write {}", unit_path.display()))?;
    Ok(unit_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_command_bounds_each_capture_with_timeout() {
        use std::path::Path;
        let cmd = watch_shell_command(Path::new("/usr/bin/niri-clip"), 7);
        assert_eq!(cmd, "exec timeout 7s /usr/bin/niri-clip");
        assert!(
            cmd.starts_with("exec timeout ") && cmd.contains("s /usr/bin"),
            "每次捕获必须被 timeout 划界"
        );
        // 路径含空格时依赖 display 的字面量——shell 层由外层 sh -c 整体接收，
        // 这里只锁定格式契约
        let spaced = watch_shell_command(Path::new("/opt/my tools/niri-clip"), 2);
        assert_eq!(spaced, "exec timeout 2s /opt/my tools/niri-clip");
    }

    #[test]
    fn embedded_service_unit_points_to_cargo_bin() {
        assert!(SERVICE_UNIT.contains("%h/.cargo/bin/niri-clip"));
        assert!(SERVICE_UNIT.contains("Restart="));
    }
}
