use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::store;

fn has_bin(name: &str) -> bool {
    which::which(name).is_ok()
}

/// fzf 功能地板：`--no-input`/`show-input` 需 0.59，`--id-nth` 需 0.71。
/// 低于地板时 fzf 参数解析直接失败，必须主动回退 fuzzel。
const FZF_MIN: (u32, u32) = (0, 71);

/// 解析 `fzf --version` 输出（如 "0.74.4 (c0252b6)"）为 (major, minor)
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let head = s.split_whitespace().next()?;
    let mut it = head.split('.');
    let major: u32 = it.next()?.parse().ok()?;
    let minor: u32 = it.next()?.parse().ok()?;
    Some((major, minor))
}

/// fzf 版本门控。`fzf --version` 每次约 10ms，而 Mod+V 是高频路径——
/// 版本号缓存到 state/fzf.version，仅当 fzf 二进制 mtime 比缓存新时重查
/// （fzf 升级自动失效重校，不会出现"升级后门控判定过期"的问题）。
fn fzf_version_ok() -> bool {
    let Ok(fzf_path) = which::which("fzf") else {
        return false;
    };
    let cache = Config::state_dir().join("fzf.version");
    let fzf_mtime = std::fs::metadata(&fzf_path).and_then(|m| m.modified()).ok();
    let cache_mtime = std::fs::metadata(&cache).and_then(|m| m.modified()).ok();
    if let (Some(f), Some(c)) = (fzf_mtime, cache_mtime) {
        if f <= c {
            let cached = std::fs::read_to_string(&cache).unwrap_or_default();
            return parse_version(&cached)
                .map(|v| v >= FZF_MIN)
                .unwrap_or(false);
        }
    }
    let Ok(out) = Command::new("fzf").arg("--version").output() else {
        return false;
    };
    let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // 缓存写失败（只读 fs 等）不影响本次判定，只是退回每次实查
    let _ = std::fs::create_dir_all(Config::state_dir());
    let _ = std::fs::write(&cache, &ver);
    parse_version(&ver).map(|v| v >= FZF_MIN).unwrap_or(false)
}

/// 原生 GUI 二进制定位（多级兜底）：PATH → 与当前可执行文件同目录
/// （cargo install 同仓同目录）→ ~/.cargo/bin。
/// 必须有兜底：niri spawn 环境的 PATH 常缺 ~/.cargo/bin（真机踩坑），
/// 否则 auto 会误降级 fzf，GUI 永远打不开。
fn gui_binary() -> Option<std::path::PathBuf> {
    if let Ok(p) = which::which("niri-clip-gui") {
        return Some(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("niri-clip-gui");
            if sibling.exists() {
                return Some(sibling);
            }
        }
    }
    let cargo = dirs::home_dir()?.join(".cargo/bin/niri-clip-gui");
    cargo.exists().then_some(cargo)
}

/// 选择后端：
/// - "native"：原生 layer-shell 窗口（niri-clip-gui 二进制），无终端依赖
/// - "fzf"：fzf 承载于终端（--track 不跳顶唯一现成解）
/// - "fuzzel"：dmenu 兜底（无 TTY 需求）
/// - "auto"：native 可用优先；其次 fzf；最后 fuzzel（fzf 版本地板在
///   run() 里二次校验，此处只查存在性）
fn backend(cfg: &Config) -> &'static str {
    match cfg.tui_backend.as_str() {
        "fuzzel" => "fuzzel",
        "fzf" => "fzf",
        "native" => "native",
        _ => {
            if gui_binary().is_some() {
                "native"
            } else if has_bin("fzf") {
                "fzf"
            } else {
                "fuzzel"
            }
        }
    }
}

/// v0.4：菜单取数统一入口——直查 min(max_items, TUI_LIMIT) 条，
/// 不再区分缓存/非缓存分支（缓存层已移除）
fn menu_clips(cfg: &Config) -> Result<Vec<store::Clip>> {
    store::list(cfg.max_items.min(store::TUI_LIMIT))
}

/// fzf 的全屏 UI 依赖控制终端（/dev/tty）。niri `spawn` 拉起的进程
/// 没有控制终端，fzf 会直接报 "inappropriate ioctl for device" 退出。
fn has_controlling_tty() -> bool {
    std::fs::File::open("/dev/tty").is_ok()
}

/// 无 TTY 时用于承载 fzf 的终端模拟器及其透传参数前缀（按序探测）。
/// 顺序即启动耗时顺序：foot 最轻；ghostty 启动明显轻于 kitty，
/// 提前探测（Mod+V 链路中终端冷启动是主要延迟来源）。
fn terminal_wrap() -> Option<(&'static str, &'static [&'static str])> {
    for (term, prefix) in [
        ("foot", &[] as &[&str]),
        ("ghostty", &["-e"]),
        ("kitty", &[]),
        ("alacritty", &["-e"]),
        ("wezterm", &["start", "--"]),
    ] {
        if has_bin(term) {
            return Some((term, prefix));
        }
    }
    None
}

/// niri `spawn` 拉起的外层进程没有控制终端：包装一层终端模拟器重跑自己。
///
/// 内层命令经 `sh -c` 把 stdout/stderr 重定向到 state/tui.log——
/// fzf 退出瞬间 alt-screen 收起，若 scrollback 里有启动日志/copied 文本，
/// 关闭时终端会闪现一帧"脚本输出"；重定向后 scrollback 干净，闪窗只剩
/// 空帧不可见。日志文件同时充当无 systemd 环境的排障入口。
fn respawn_in_terminal(term: &str, prefix: &[&str], exe: &std::path::Path) -> Result<()> {
    let log = Config::state_dir().join("tui.log");
    let inner = format!("exec '{}' tui >> '{}' 2>&1", exe.display(), log.display());
    Command::new(term)
        .args(prefix)
        .args(["sh", "-c", &inner])
        .spawn()
        .with_context(|| format!("spawn {term} to host fzf"))?;
    Ok(())
}

pub fn run() -> Result<()> {
    let cfg = Config::load();
    let mut be = backend(&cfg);

    // native：layer-shell 进程无需 TTY，直接拉起独立 GUI 后返回。
    // 二进制缺失时降级：显式 "native" → fzf/fuzzel；auto 已在此前选择过。
    if be == "native" {
        return match gui_binary() {
            Some(gui) => {
                Command::new(&gui)
                    .spawn()
                    .with_context(|| format!("spawn {}", gui.display()))?;
                Ok(())
            }
            None => {
                eprintln!("[niri-clip tui] niri-clip-gui 不可用，回退 fzf/fuzzel");
                be = if has_bin("fzf") { "fzf" } else { "fuzzel" };
                run_fallback(be, &cfg)
            }
        };
    }

    // fzf/fuzzel：无控制终端（niri spawn 裸拉起）时先做 tty 探测——
    // fzf 需要包一层终端；version 检查留给实际执行 fzf 的路径
    if !has_controlling_tty() {
        if let Some((term, prefix)) = terminal_wrap() {
            let exe = std::env::current_exe()?;
            return respawn_in_terminal(term, prefix, &exe);
        }
        if has_bin("fuzzel") {
            eprintln!("[niri-clip tui] 无可用终端承载 fzf，回退 fuzzel");
            return run_fuzzel(&cfg);
        }
        if cfg.notify_enabled {
            let _ = notify_rust::Notification::new()
                .summary("niri-clip")
                .body("fzf 需要 TTY：请安装 foot/ghostty/kitty 等终端，或安装 fuzzel")
                .show();
        }
        anyhow::bail!("no controlling tty and no terminal emulator / fuzzel available");
    }

    // fzf 版本地板：低于 0.71（--id-nth）时参数无法解析，主动回退 fuzzel
    if be == "fzf" && !fzf_version_ok() {
        if has_bin("fuzzel") {
            eprintln!("[niri-clip tui] fzf 缺失或 <0.71（--id-nth 地板），回退 fuzzel");
            be = "fuzzel";
        } else {
            if cfg.notify_enabled {
                let _ = notify_rust::Notification::new()
                    .summary("niri-clip")
                    .body("niri-clip 需要 fzf >= 0.71（或安装 fuzzel）")
                    .show();
            }
            anyhow::bail!("fzf missing or too old (<0.71) and fuzzel unavailable");
        }
    }
    eprintln!("[niri-clip tui] backend={} db={:?}", be, Config::db_path());
    match be {
        "fzf" => run_fzf(&cfg),
        _ => run_fuzzel(&cfg),
    }
}

/// native 不可用时的降级执行：仍处于无 TTY 环境的决策辅助
fn run_fallback(be: &'static str, cfg: &Config) -> Result<()> {
    match be {
        "fuzzel" => run_fuzzel(cfg),
        "fzf" => {
            // 无 TTY 时 fzf 无法运行：包终端重跑（backend 已定型 fzf）
            if has_controlling_tty() {
                return run_fzf(cfg);
            }
            if let Some((term, prefix)) = terminal_wrap() {
                let exe = std::env::current_exe()?;
                return respawn_in_terminal(term, prefix, &exe);
            }
            anyhow::bail!("fzf 需要 TTY 且无可用终端模拟器");
        }
        _ => run_fuzzel(cfg),
    }
}

/// fzf/fuzzel 共用的行渲染：序号 + 当前项标记(▶) + 星标(★) + id + 预览。
/// fzf 输入列布局（tab 分隔）：1=num 2=cur 3=star 4=id 5=preview。
/// ▶ 语义 = 最后一次复制的内容 ≈ Ctrl+V 会粘出的东西（store::current_hash）。
fn row_marks(cur: Option<&str>, c: &store::Clip) -> (String, String) {
    let cur_mark = if cur == Some(c.hash.as_str()) {
        "▶"
    } else {
        " "
    };
    let star = if c.pinned { "★" } else { " " };
    (cur_mark.to_string(), star.to_string())
}

fn run_fzf(cfg: &Config) -> Result<()> {
    let clips = menu_clips(cfg)?;
    if clips.is_empty() {
        if cfg.notify_enabled {
            let _ = notify_rust::Notification::new()
                .summary("niri-clip")
                .body("剪贴板历史为空")
                .show();
        }
        return Ok(());
    }

    let cur = store::current_hash();
    // 生成 fzf 输入：序号\t▶\t★\t{id}\t{preview}  (序号用于 1-9 快选定位)
    let mut input = String::new();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let (cur_mark, star) = row_marks(cur.as_deref(), c);
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        input.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            num, cur_mark, star, c.id, preview
        ));
    }

    // header 缺席提示：指针存在但列表第 1 行不匹配 → 当前内容被过滤/超限，
    // 不在历史中（在库中则必因置顶排序出现在第 1 行）
    let mut header =
        "Alt+1..9快选 · Space跳 · /或Ctrl-F搜索 · Enter复制 · Ctrl-Y不退出 · ▶=当前".to_string();
    if cur
        .as_ref()
        .is_some_and(|h| clips.first().is_some_and(|c| c.hash != *h))
    {
        header.push_str(" · 当前剪贴板不在历史中(被过滤或超限)");
    }

    // 临时脚本目录
    let cache = dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("niri-clip");
    std::fs::create_dir_all(&cache)?;
    let exe = std::env::current_exe()?.to_string_lossy().to_string();

    // 构建 fzf 参数：单进程 reload-sync + track + id-nth
    // 注意：reload 命令需要重新从 DB 读取，所以用 `niri-clip list-raw` 子命令
    let reload_cmd = format!("{} list-raw", exe);
    let pin_cmd = format!("{} pin {{4}}", exe);
    let del_cmd = format!("{} delete {{4}}", exe);
    let wipe_cmd = format!("{} wipe", exe);

    let preview_cmd = if cfg.enable_preview {
        // id 在第 4 列 (序号、▶、★、id、预览)
        format!("{} preview {{4}}", exe)
    } else {
        "echo {5..}".to_string()
    };

    // A+B: Alt+1..9 快选 + Space jump + / 和 Ctrl-F 搜索 (裸数字留给搜索输入)
    let mut binds: Vec<String> = Vec::new();
    for n in 1..=9 {
        binds.push(format!("alt-{n}:pos({n})+accept"));
    }
    binds.push("space:jump".into());
    binds.push("ctrl-y:execute-silent(niri-clip copy {4})".into());

    let mut fzf = Command::new("fzf")
        .arg("--no-sort")
        .arg("--delimiter=\t")
        // 匹配只作用于 preview 列：隐藏的序号/标记/id 列不参与搜索，
        // 否则查询 "1" 会命中所有序号/id 含 1 的行，数字搜索被污染
        .arg("--nth=5..")
        .arg("--with-nth=1,2,3,5..")
        // 快选/跳转/不退出复制：binds 必须显式挂载，
        // 否则 Alt+1..9 等于没有绑定，快选静默失效
        .arg(format!("--bind={}", binds.join(",")))
        .arg("--tabstop=1")
        .arg("--height=100%")
        .arg("--layout=reverse")
        .arg("--border")
        .arg("--info=inline")
        .arg("--prompt=剪贴板> ")
        .arg(format!("--header={header}"))
        .arg("--track")
        .arg("--id-nth=4")
        .arg("--no-input")
        .arg(format!("--preview={}", preview_cmd))
        .arg("--preview-window=down:5:wrap:border-rounded")
        .arg("--bind=/:show-input")
        .arg("--bind=ctrl-f:show-input")
        .arg("--bind=esc:abort")
        .arg(format!(
            "--bind=ctrl-p:execute-silent({})+reload-sync({})",
            pin_cmd, reload_cmd
        ))
        .arg(format!(
            "--bind=ctrl-x:execute-silent({})+reload-sync({})",
            del_cmd, reload_cmd
        ))
        .arg(format!("--bind=ctrl-r:reload-sync({})", reload_cmd))
        .arg(format!(
            "--bind=alt-x:execute({})+reload-sync({})",
            wipe_cmd, reload_cmd
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("spawn fzf")?;

    {
        let stdin = fzf.stdin.as_mut().unwrap();
        stdin.write_all(input.as_bytes())?;
    }
    let out = fzf.wait_with_output()?;
    let selected = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if selected.is_empty() {
        return Ok(());
    }
    // fzf 输出是 选中的行，格式同输入
    // 可能包含多个? 只取第一行
    let line = selected.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 4 {
        return Ok(());
    }
    let id: i64 = parts[3].parse().unwrap_or(0);
    if id == 0 {
        return Ok(());
    }
    let clip = store::get(id)?;
    // Enter 复制即成为"当前内容"：刷新指针，下次打开 ▶ 指向它
    // （正常情况下 watch 捕获也会刷新，这里显式写保证即时一致）
    store::touch_current(&clip.hash);
    // wl-copy
    let mut wl = Command::new("wl-copy")
        .stdin(Stdio::piped())
        // 关键：wl-copy 会 fork 守护进程常驻服务剪贴板，默认继承的
        // 终端 fd 被守护进程一直占着，kitty 等 pty EOF 才关窗，
        // 导致 TUI 退出后残留黑屏空窗口。重定向到 null 释放 pty。
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("wl-copy")?;
    wl.stdin.as_mut().unwrap().write_all(clip.text.as_bytes())?;
    wl.wait()?;
    println!("copied {}", id);
    Ok(())
}

fn run_fuzzel(cfg: &Config) -> Result<()> {
    let clips = menu_clips(cfg)?;
    if clips.is_empty() {
        return Ok(());
    }
    let cur = store::current_hash();
    let mut input = String::new();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let (cur_mark, star) = row_marks(cur.as_deref(), c);
        let marks = format!("{cur_mark}{star}").trim().to_string();
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        input.push_str(&format!("{}. {} {} {}\n", num, marks, c.id, preview));
    }
    // fuzzel dmenu 简单实现：选中后粘贴，不支持原地 reload，按 Enter 后退出
    // v0.2 fuzzel 模式为兜底，v1.0 再做 fuzzel 原地刷新
    let mut child = Command::new("fuzzel")
        .arg("--dmenu")
        .arg("--prompt=剪贴板> ")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("fuzzel")?;
    child.stdin.as_mut().unwrap().write_all(input.as_bytes())?;
    let out = child.wait_with_output()?;
    let sel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sel.is_empty() {
        return Ok(());
    }
    // 行格式 "{num}. {marks} {id} {preview}"：marks 含 ▶/★ 前缀符号，
    // 直接取首个可解析为整数的 token 作 id（对符号增减鲁棒）
    let id: i64 = sel
        .split_whitespace()
        .find_map(|t| t.parse::<i64>().ok())
        .unwrap_or(0);
    if id == 0 {
        return Ok(());
    }
    let clip = store::get(id)?;
    store::touch_current(&clip.hash);
    let mut wl = Command::new("wl-copy").stdin(Stdio::piped()).spawn()?;
    wl.stdin.as_mut().unwrap().write_all(clip.text.as_bytes())?;
    wl.wait()?;
    Ok(())
}

/// 子命令辅助：list-raw 供 fzf reload 调用（行格式须与 run_fzf 初始输入一致）
pub fn list_raw() -> Result<()> {
    let cfg = Config::load();
    let clips = menu_clips(&cfg)?;
    let cur = store::current_hash();
    use std::io::{self, Write};
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let (cur_mark, star) = row_marks(cur.as_deref(), c);
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        if writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            num, cur_mark, star, c.id, preview
        )
        .is_err()
        {
            break;
        }
    }
    Ok(())
}

pub fn preview_id(id: i64) -> Result<()> {
    let c = store::get(id)?;
    // v0.3: 图片预览分支
    if c.mime.starts_with("image/") {
        let cfg = Config::load();
        if cfg.enable_image_preview {
            let rendered = crate::preview::render_preview(&c);
            if !rendered.is_empty() {
                println!("{}", rendered);
                return Ok(());
            }
        }
        println!("[image {}]", c.mime);
        println!("{}", c.text);
        return Ok(());
    }
    // 文本：输出全量，截断 2000 字符 + 100 行
    for line in c.text.lines().take(100) {
        let l: String = line.chars().take(300).collect();
        println!("{}", l);
    }
    if c.text.len() > 2000 {
        println!("… ({} bytes)", c.text.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_handles_fzf_output() {
        assert_eq!(parse_version("0.74.4 (c0252b6)"), Some((0, 74)));
        assert_eq!(parse_version("0.71.0"), Some((0, 71)));
        assert_eq!(parse_version("0.59.0 (deb5468)"), Some((0, 59)));
    }

    #[test]
    fn parse_version_rejects_garbage() {
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn version_gate_matches_feature_floor() {
        // --id-nth 地板 0.71：0.70 拦截，0.71/0.74 放行
        assert!((0, 70) < FZF_MIN);
        assert!((0, 71) >= FZF_MIN);
        assert!((0, 74) >= FZF_MIN);
    }
}
