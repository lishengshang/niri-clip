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
    "/../../assets/niri-clip.service"
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
    format!("exec timeout {timeout_secs}s {} store", exe.display())
}

/// 一路 wl-paste --watch 的完整参数表。单列出来以便单元测试覆盖
/// PRIMARY 监视的开关语义（任务 1.1）。
fn wl_paste_watch_args(exe: &std::path::Path, timeout_secs: u64, primary: bool) -> Vec<String> {
    let mut args = vec!["--watch".to_string()];
    if primary {
        // 主选区：鼠标划选即触发（中键粘贴语义）；与剪贴板 watcher 各自独立
        args.push("--primary".to_string());
    }
    args.push("sh".to_string());
    args.push("-c".to_string());
    args.push(watch_shell_command(exe, timeout_secs));
    args
}

fn spawn_watch(
    exe: &std::path::Path,
    timeout_secs: u64,
    primary: bool,
) -> Result<tokio::process::Child> {
    tokio::process::Command::new("wl-paste")
        .args(wl_paste_watch_args(exe, timeout_secs, primary))
        .spawn()
        .context("spawn wl-paste --watch")
}

/// 超限提示：stderr 恒写；桌面通知受 notify_enabled 门控（P1-4，无通知服务时静默）
fn notify_oversize(msg: &str) {
    eprintln!("[niri-clip store] {msg}，已忽略");
    if Config::load().notify_enabled {
        crate::notify::send(msg);
    }
}

/// `niri-clip store` : 入库一段剪贴板载荷。
///
/// * stdin 有数据（主模式：wl-paste 管道直灌）→ 先按 magic 判定图片
///   （png/jpeg/webp，任务 4.1），命中即按 `max_image_bytes` 裁决入库；
///   否则按 `max_clip_bytes` 裁决文本，超限且剪贴板另有图片 MIME 时才回头
///   走图片探测（见超限分支注释）；
/// * stdin 为空（历史兼容：直接手动执行 store）→ 保持旧的
///   get_contents(Text) 探测，并在开启图片预览时尝试图片 MIME。
///
/// v0.5 限流：读取用 `Read::take(max+1)` 划界——读满 max+1 即判定超限
/// 整体拒绝，未超限时 take 内已到 EOF 载荷完整；杜绝超大载荷全内存直通
/// （文本路径内存上限 = max_clip_bytes + 1，图片路径 = max_image_bytes + 1）。
pub fn store_from_stdin() -> Result<()> {
    let cfg = Config::load();
    let text_cap = cfg.max_clip_bytes as u64;
    let mut stdin = std::io::stdin().lock();
    let mut buf = Vec::new();
    stdin.by_ref().take(text_cap + 1).read_to_end(&mut buf)?;

    // 图片：stdin 不带 MIME（wl-paste watch 在无文本可给时按 try_any 兜底
    // 输出任意类型），只能靠 magic 判定。先按文本限额读够判定所需的头，命中
    // 后再**沿同一 stdin 续读**到图片限额——单次读取直接入库，不再"丢弃字节
    // + 回头再读一次剪贴板"（第二次读到的不一定是触发时那份内容，且来源应用
    // 已退出时直接失败）。内存上限仍是该分支自己的限额 +1。
    if cfg.enable_image_preview {
        if let Some(mime) = sniff_image_mime(&buf) {
            let image_cap = cfg.max_image_bytes as u64;
            if (buf.len() as u64) <= image_cap {
                let rest = image_cap + 1 - buf.len() as u64;
                stdin.by_ref().take(rest).read_to_end(&mut buf)?;
            }
            ingest_image(mime, &buf, &cfg)?;
            return Ok(());
        }
    }

    if buf.len() as u64 > cfg.max_clip_bytes as u64 {
        // 已由 magic 判定过的图片在上面直接入库；走到这里的是**未识别**的
        // 二进制（bmp/svg 等图片格式、或任意大文件）。图省事一律拒绝会误杀
        // 图片，故开启图片捕获且剪贴板确实提供受支持 MIME 时，交图片路径按
        // max_image_bytes 重新裁决（其超限通知自行负责，此处不再叠加文本
        // 报错）；图片不可走时才按文本超限拒绝。
        if cfg.enable_image_preview && clipboard_offers_image() {
            capture_image_if_enabled()?;
            return Ok(());
        }
        notify_oversize(&format!(
            "条目超过 max_clip_bytes={} 字节",
            cfg.max_clip_bytes
        ));
        return Ok(());
    }

    // 非空载荷：优先视作 UTF-8 文本
    if !buf.is_empty() {
        match String::from_utf8(buf) {
            Ok(text) => {
                ingest_text(&text, &cfg)?;
                return Ok(());
            }
            Err(broken) => {
                // 非 UTF-8 且不是受支持的图片 magic：不在文本语义里硬塞，
                // 只记录并交给 stderr 排障（下方再按图片 MIME 探测一次）
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

/// 文本入库（含 ignore 规则），统一入口便于测试。复用调用方已加载的配置
fn ingest_text(text: &str, cfg: &Config) -> Result<bool> {
    let inserted = store::insert_with(text.to_string(), None, cfg)?;
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
    let cfg = Config::load();
    let max = cfg.max_clip_bytes;
    let cap = max as u64 + 1;
    match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
        Ok((pipe, _)) => {
            let mut v = Vec::new();
            if pipe.take(cap).read_to_end(&mut v).is_ok() {
                if v.len() as u64 > max as u64 {
                    notify_oversize(&format!("条目超过 max_clip_bytes={max} 字节"));
                    return Ok(false);
                }
                let text = String::from_utf8(v)
                    .map_err(|_| anyhow!("clipboard payload is not valid utf-8"))?;
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return ingest_text(trimmed, &cfg);
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

/// 受支持的图片 MIME，探测序即尝试序（png 优先——截图场景的主型）。
const IMAGE_MIMES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];

/// 按文件头（magic）判断 stdin 载荷是否为受支持的图片。
///
/// 需要它的原因：`wl-paste --watch` 的 `--type` 是**单值**（上游 `wl-paste.c`
/// 里 `case 't': options.explicit_type = strdup(optarg)`，重复给出只有最后一个
/// 生效），而显式 `--type text` 在"剪贴板只有图片"时会直接失败、根本不触发
/// 命令。现有实现因此不给 `--type`——上游 `mime_type_to_request()` 在无显式
/// 类型时按 text/plain;charset=utf-8 → text/plain → 任意 text → **任意类型**
/// 兜底，图片剪贴板于是以原始字节直灌 stdin，但 stdin 不带 MIME。
///
/// 判定只用头部字节，不做完整格式校验：真正的解码在预览/GUI 侧，入库阶段
/// 只负责"归到哪条存储路径"。未知格式返回 `None`，由调用方走旧的剪贴板
/// MIME 探测（受支持的三种之外的图片不猜）。
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        // JPEG 以 SOI(FFD8) + 任意 marker(FFxx) 开头
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// 图片入库的唯一落点：stdin magic 路径与剪贴板 MIME 路径共用，
/// 保证两条入口的超限文案与日志格式一致。
fn ingest_image(mime: &str, bytes: &[u8], cfg: &Config) -> Result<bool> {
    if bytes.len() > cfg.max_image_bytes {
        notify_oversize(&format!(
            "图片超过 max_image_bytes={} 字节",
            cfg.max_image_bytes
        ));
        return Ok(false);
    }
    match store::insert_image_with(mime, bytes, cfg) {
        Ok(Some(img)) => {
            eprintln!(
                "[niri-clip store] stored image #{} ({mime}, {} B) -> {}",
                img.id,
                bytes.len(),
                img.path.display()
            );
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(e) => Err(anyhow!("insert image: {e:#}")),
    }
}

/// 剪贴板是否提供任一受支持的图片 MIME。仅探测可用性不读取内容：
/// 超限复查路径只需知道"图片可走"，真正的读取与限额裁决交给
/// [`capture_image_if_enabled`]。
fn clipboard_offers_image() -> bool {
    IMAGE_MIMES.iter().any(|mime| {
        get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            MimeType::Specific(mime),
        )
        .is_ok()
    })
}

fn capture_image_if_enabled() -> Result<bool> {
    let cfg = Config::load();
    if !cfg.enable_image_preview {
        return Ok(false);
    }
    let cap = cfg.max_image_bytes as u64 + 1;
    for mime in IMAGE_MIMES {
        if let Ok((pipe, _)) = get_contents(
            ClipboardType::Regular,
            Seat::Unspecified,
            MimeType::Specific(mime),
        ) {
            let mut v = Vec::new();
            if pipe.take(cap).read_to_end(&mut v).is_ok() && !v.is_empty() {
                return ingest_image(mime, &v, &cfg);
            }
        }
    }
    Ok(false)
}

/// v0.4.1 主模式：事件驱动外部源。capture_primary 时额外拉起一路
/// PRIMARY selection 监视（任务 1.1）——划选文本也入库。
async fn run_watch(timeout_secs: u64, capture_primary: bool) -> Result<()> {
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    println!(
        "[niri-clip daemon] event-driven source: wl-paste --watch (per-capture timeout {timeout_secs}s)"
    );
    if Config::load().notify_enabled {
        crate::notify::send("守护进程已启动 (event)");
    }

    let exe_path = std::path::Path::new(&exe);
    let mut clip_watcher = spawn_watch(exe_path, timeout_secs, false)?;
    let mut primary_watcher = if capture_primary {
        println!("[niri-clip daemon] also watching PRIMARY selection (capture_primary=true)");
        Some(spawn_watch(exe_path, timeout_secs, true)?)
    } else {
        None
    };
    println!("[niri-clip daemon] watching clipboard changes ...");
    match primary_watcher.as_mut() {
        None => {
            let status = clip_watcher.wait().await?;
            eprintln!("[niri-clip daemon] wl-paste exited: {status:?}");
        }
        Some(primary) => {
            let (a, b) = tokio::join!(clip_watcher.wait(), primary.wait());
            eprintln!("[niri-clip daemon] wl-paste exited: {a:?} / {b:?}");
        }
    }
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
    let max = Config::load().max_clip_bytes;
    let cap = max as u64 + 1;
    let mut last_hash: Option<String> = None;
    loop {
        match get_contents(ClipboardType::Regular, Seat::Unspecified, MimeType::Text) {
            Ok((pipe, _)) => {
                let mut v = Vec::new();
                if pipe.take(cap).read_to_end(&mut v).is_ok() {
                    if v.len() as u64 > max as u64 {
                        // 超限内容会持续占据剪贴板：以内容前缀 hash 短路，
                        // 避免每 500ms 重复通知
                        let key = format!(
                            "oversize:{}",
                            store::hash_text(&format!("{max}:{}", v.len()))
                        );
                        if last_hash.as_ref() != Some(&key) {
                            last_hash = Some(key);
                            notify_oversize(&format!("条目超过 max_clip_bytes={max} 字节"));
                        }
                        sleep(Duration::from_millis(500)).await;
                        continue;
                    }
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

/// 单实例锁（机制在 `core::single_instance`，任务 2.6 收敛为全项目唯一实现）。
/// daemon 被占用时报错退出——"已有实例"对 daemon 是异常，对 GUI 则是常态
/// （连按 Mod+V 应聚焦已开窗口），故占用处理留在调用方。
fn acquire_single_instance() -> Result<crate::single_instance::InstanceGuard> {
    match crate::single_instance::InstanceGuard::try_acquire("daemon")? {
        Some(guard) => Ok(guard),
        None => Err(anyhow!(
            "另一个 niri-clip daemon 正在运行（锁 {}）",
            Config::state_dir().join("daemon.lock").display()
        )),
    }
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
        "[niri-clip daemon] max_items={} tui={} image_preview={} max_clip_bytes={} max_image_bytes={}",
        cfg.max_items, cfg.tui_backend, cfg.enable_image_preview, cfg.max_clip_bytes, cfg.max_image_bytes
    );
    println!("[niri-clip daemon] db: {}", Config::db_path().display());

    // 启动时回收孤儿图片文件（旧版本 delete/wipe/淘汰不删文件的存量残留
    // 与入库中途崩溃的 .tmp- 残片）。失败不阻断捕获，仅记日志。
    match store::prune_orphan_images() {
        Ok(0) => {}
        Ok(n) => println!("[niri-clip daemon] 清理孤儿图片文件 {n} 个"),
        Err(e) => eprintln!("[niri-clip daemon] prune orphan images failed: {e:#}"),
    }

    // 图片磁盘配额 GC（1.3）：超 max_image_total_bytes 按 LRU 淘汰最旧图片
    // 条目（星标/当前项保护）。失败不阻断捕获，仅记日志
    match store::gc_images(cfg.max_image_total_bytes as u64) {
        Ok(0) => {}
        Ok(n) => println!("[niri-clip daemon] 图片配额 GC 淘汰 {n} 条"),
        Err(e) => eprintln!("[niri-clip daemon] image quota gc failed: {e:#}"),
    }

    if which::which("wl-paste").is_ok() {
        return run_watch(cfg.capture_timeout_secs, cfg.capture_primary).await;
    }

    eprintln!("[warn] missing wl-paste —— 回退到原生轮询模式");
    if which::which("wl-copy").is_err() {
        eprintln!("[warn] missing wl-copy");
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
        assert_eq!(cmd, "exec timeout 7s /usr/bin/niri-clip store");
        assert!(
            cmd.starts_with("exec timeout ") && cmd.contains("s /usr/bin"),
            "每次捕获必须被 timeout 划界"
        );
        // 路径含空格时依赖 display 的字面量——shell 层由外层 sh -c 整体接收，
        // 这里只锁定格式契约
        let spaced = watch_shell_command(Path::new("/opt/my tools/niri-clip"), 2);
        assert_eq!(spaced, "exec timeout 2s /opt/my tools/niri-clip store");
    }

    #[test]
    fn primary_watch_adds_selection_flag() {
        use std::path::Path;
        let exe = Path::new("/usr/bin/niri-clip");
        let plain = wl_paste_watch_args(exe, 5, false);
        let primary = wl_paste_watch_args(exe, 5, true);
        assert!(!plain.contains(&"--primary".to_string()));
        assert!(primary.contains(&"--primary".to_string()));
        // PRIMARY watcher 的每次捕获同样被 timeout 划界
        assert!(primary.last().unwrap().contains("timeout 5s"));
        // 除 --primary 外两路参数完全一致
        let mut a = plain.clone();
        let mut b = primary.clone();
        a.retain(|x| x != "--primary");
        b.retain(|x| x != "--primary");
        assert_eq!(a, b);
    }

    #[test]
    fn embedded_service_unit_points_to_cargo_bin() {
        assert!(SERVICE_UNIT.contains("%h/.cargo/bin/niri-clip"));
        assert!(SERVICE_UNIT.contains("Restart="));
    }

    #[test]
    fn sniffs_the_three_supported_image_magics() {
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
        let jpeg = b"\xFF\xD8\xFF\xE0\x00\x10JFIF";
        let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
        assert_eq!(sniff_image_mime(png), Some("image/png"));
        assert_eq!(sniff_image_mime(jpeg), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(webp), Some("image/webp"));
    }

    #[test]
    fn sniff_ignores_unknown_and_truncated_payloads() {
        // 未知二进制格式交回旧的剪贴板 MIME 探测，不猜
        assert_eq!(sniff_image_mime(b"BM\x00\x00\x00\x00"), None);
        assert_eq!(sniff_image_mime(b"RIFF\x24\x00\x00\x00WAVE"), None);
        assert_eq!(sniff_image_mime(b"RIFF"), None);
        // 文本永不命中（PNG magic 首字节 0x89 不是合法 UTF-8 起始）
        assert_eq!(sniff_image_mime(b"hello world"), None);
        assert_eq!(sniff_image_mime(b""), None);
    }
}
