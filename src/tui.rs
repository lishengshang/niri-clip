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

fn fzf_version_ok() -> bool {
    let Ok(out) = Command::new("fzf").arg("--version").output() else {
        return false;
    };
    parse_version(&String::from_utf8_lossy(&out.stdout))
        .map(|v| v >= FZF_MIN)
        .unwrap_or(false)
}

/// 选择后端：auto 时优先 fzf（任意终端均可运行），缺失则 fuzzel。
/// v0.4 修复：旧实现要求 `fzf && kitty` 同时存在才启用 fzf，导致
/// foot/alacritty 等用户被误降到功能残缺的 fuzzel。kitty/chafa 仅
/// 影响图片预览渲染能力，不应决定整个后端选择。
fn backend(cfg: &Config) -> &'static str {
    match cfg.tui_backend.as_str() {
        "fuzzel" => "fuzzel",
        "fzf" => "fzf",
        _ => {
            if has_bin("fzf") {
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

/// 无 TTY 时用于承载 fzf 的终端模拟器及其透传参数前缀（按序探测）
fn terminal_wrap() -> Option<(&'static str, &'static [&'static str])> {
    for (term, prefix) in [
        ("foot", &[] as &[&str]),
        ("alacritty", &["-e"]),
        ("kitty", &[]),
        ("wezterm", &["start", "--"]),
        ("ghostty", &["-e"]),
    ] {
        if has_bin(term) {
            return Some((term, prefix));
        }
    }
    None
}

pub fn run() -> Result<()> {
    let cfg = Config::load();
    let mut be = backend(&cfg);
    // fzf 版本地板：低于 0.71（--id-nth）时参数无法解析，主动回退 fuzzel
    if be == "fzf" && !fzf_version_ok() {
        if has_bin("fuzzel") {
            eprintln!("[niri-clip tui] fzf 缺失或 <0.71（--id-nth 地板），回退 fuzzel");
            be = "fuzzel";
        } else {
            let _ = notify_rust::Notification::new()
                .summary("niri-clip")
                .body("niri-clip 需要 fzf >= 0.71（或安装 fuzzel）")
                .show();
            anyhow::bail!("fzf missing or too old (<0.71) and fuzzel unavailable");
        }
    }
    // 无控制终端（如 niri spawn 裸拉起）时 fzf 无法运行：
    // 包一层终端模拟器重跑自己；没有终端则回退 fuzzel（无需 TTY）
    if be == "fzf" && !has_controlling_tty() {
        if let Some((term, prefix)) = terminal_wrap() {
            let exe = std::env::current_exe()?;
            Command::new(term)
                .args(prefix)
                .arg(exe)
                .arg("tui")
                .spawn()
                .context("spawn terminal to host fzf")?;
            return Ok(());
        }
        if has_bin("fuzzel") {
            eprintln!("[niri-clip tui] 无可用终端承载 fzf，回退 fuzzel");
            return run_fuzzel(&cfg);
        }
        let _ = notify_rust::Notification::new()
            .summary("niri-clip")
            .body("fzf 需要 TTY：请安装 foot/alacritty/kitty 等终端，或安装 fuzzel")
            .show();
        anyhow::bail!("no controlling tty and no terminal emulator / fuzzel available");
    }
    eprintln!("[niri-clip tui] backend={} db={:?}", be, Config::db_path());
    match be {
        "fzf" => run_fzf(&cfg),
        _ => run_fuzzel(&cfg),
    }
}

fn run_fzf(cfg: &Config) -> Result<()> {
    let clips = menu_clips(cfg)?;
    if clips.is_empty() {
        let _ = notify_rust::Notification::new()
            .summary("niri-clip")
            .body("剪贴板历史为空")
            .show();
        return Ok(());
    }

    // 生成 fzf 输入：序号\t★\t{id}\t{preview}  (序号用于 1-9 快选定位)
    let mut input = String::new();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let star = if c.pinned { "★" } else { " " };
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        // 显示序号 1-99，fzf 用 with-nth 1,2,4.. 展示序号、星标、预览
        input.push_str(&format!("{}\t{}\t{}\t{}\n", num, star, c.id, preview));
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
    let pin_cmd = format!("{} pin {{3}}", exe);
    let del_cmd = format!("{} delete {{3}}", exe);
    let wipe_cmd = format!("{} wipe", exe);

    let preview_cmd = if cfg.enable_preview {
        // id 在第 3 列 (序号、星标、id、预览)
        format!("{} preview {{3}}", exe)
    } else {
        "echo {4..}".to_string()
    };

    // A+B: Alt+1..9 快选 + Space jump + / 和 Ctrl-F 搜索 (裸数字留给搜索输入)
    let mut binds: Vec<String> = Vec::new();
    for n in 1..=9 {
        binds.push(format!("alt-{n}:pos({n})+accept"));
    }
    binds.push("space:jump".into());
    binds.push("ctrl-y:execute-silent(niri-clip copy {3})".into());

    let mut fzf = Command::new("fzf")
        .arg("--no-sort")
        .arg("--delimiter=\t")
        // 匹配只作用于 preview 列：隐藏的序号/id 列不参与搜索，
        // 否则查询 "1" 会命中所有序号/id 含 1 的行，数字搜索被污染
        .arg("--nth=4..")
        .arg("--with-nth=1,2,4..")
        .arg("--tabstop=1")
        .arg("--height=100%")
        .arg("--layout=reverse")
        .arg("--border")
        .arg("--info=inline")
        .arg("--prompt=剪贴板> ")
        .arg("--header=Alt+1..9快选 · Space跳 · /或Ctrl-F搜索 · Enter复制 · Ctrl-Y不退出")
        .arg("--track")
        .arg("--id-nth=3")
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
    if parts.len() < 3 {
        return Ok(());
    }
    let id: i64 = parts[2].parse().unwrap_or(0);
    if id == 0 {
        return Ok(());
    }
    let clip = store::get(id)?;
    // wl-copy
    let mut wl = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .spawn()
        .context("wl-copy")?;
    wl.stdin.as_mut().unwrap().write_all(clip.text.as_bytes())?;
    wl.wait()?;
    println!("pasted {}", id);
    Ok(())
}

fn run_fuzzel(cfg: &Config) -> Result<()> {
    let clips = menu_clips(cfg)?;
    if clips.is_empty() {
        return Ok(());
    }
    let mut input = String::new();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let star = if c.pinned { "★ " } else { "" };
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        input.push_str(&format!("{}. {}{} {}\n", num, star, c.id, preview));
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
    // 解析 id：格式 "★ 123 preview" 或 "123 preview"
    let id_str = sel
        .trim_start_matches('★')
        .split_whitespace()
        .next()
        .unwrap_or("0");
    let id: i64 = id_str.parse().unwrap_or(0);
    if id == 0 {
        return Ok(());
    }
    let clip = store::get(id)?;
    let mut wl = Command::new("wl-copy").stdin(Stdio::piped()).spawn()?;
    wl.stdin.as_mut().unwrap().write_all(clip.text.as_bytes())?;
    wl.wait()?;
    Ok(())
}

/// 子命令辅助：list-raw 供 fzf reload 调用
pub fn list_raw() -> Result<()> {
    let cfg = Config::load();
    let clips = menu_clips(&cfg)?;
    use std::io::{self, Write};
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for (idx, c) in clips.iter().enumerate() {
        let num = idx + 1;
        let star = if c.pinned { "★" } else { " " };
        let preview = crate::preview::preview_text(&c, cfg.preview_width);
        if writeln!(out, "{}\t{}\t{}\t{}", num, star, c.id, preview).is_err() {
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
