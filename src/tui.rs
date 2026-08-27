use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::store;

fn has_bin(name: &str) -> bool {
    which::which(name).is_ok()
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

pub fn run() -> Result<()> {
    let cfg = Config::load();
    let be = backend(&cfg);
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

    // A+B: 1-9 快选 + Space jump + / 和 Ctrl-F 搜索
    let mut binds: Vec<String> = Vec::new();
    // 数字 1-9：输入框未启用时直接 pos+accept 退出，启用时则输入数字
    for n in 1..=9 {
        binds.push(format!(
            "{n}:transform:if [ \"$FZF_INPUT_STATE\" = \"enabled\" ]; then echo \"put({n})\"; else echo \"pos({n})+accept\"; fi"
        ));
        binds.push(format!("alt-{n}:pos({n})+accept"));
    }
    binds.push("space:jump".into());
    binds.push("ctrl-y:execute-silent(niri-clip copy {3})".into());

    let mut fzf = Command::new("fzf")
        .arg("--no-sort")
        .arg("--delimiter=\t")
        .arg("--with-nth=1,2,4..")
        .arg("--tabstop=1")
        .arg("--height=100%")
        .arg("--layout=reverse")
        .arg("--border")
        .arg("--info=inline")
        .arg("--prompt=剪贴板> ")
        .arg("--header=1-9快选 · Space跳 · /或Ctrl-F搜索 · Enter复制 · Ctrl-Y不退出")
        .arg("--track")
        .arg("--id-nth=3")
        .arg("--no-input")
        .arg(format!("--preview={}", preview_cmd))
        .arg("--preview-window=down:5:wrap:border-rounded")
        .arg("--bind=/:show-input+clear-query")
        .arg("--bind=ctrl-f:show-input+clear-query")
        .arg("--bind=esc:transform:if [ \"$FZF_INPUT_STATE\" = \"enabled\" ]; then echo \"hide-input+clear-query\"; else echo \"abort\"; fi")
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
