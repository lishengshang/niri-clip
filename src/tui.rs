use anyhow::{Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::store;

fn has_bin(name: &str) -> bool {
    which::which(name).is_ok()
}

/// 选择后端：auto 时优先 fzf+kitty，缺失则 fuzzel
fn backend(cfg: &Config) -> &'static str {
    match cfg.tui_backend.as_str() {
        "fuzzel" => "fuzzel",
        "fzf" => "fzf",
        _ => {
            if has_bin("fzf") && has_bin("kitty") {
                "fzf"
            } else if has_bin("fuzzel") {
                "fuzzel"
            } else if has_bin("fzf") {
                "fzf"
            } else {
                "fuzzel"
            }
        }
    }
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
    // 检查 fzf 版本是否支持 --track --id-nth
    let clips = store::list(cfg.max_items)?;
    if clips.is_empty() {
        let _ = notify_rust::Notification::new()
            .summary("niri-clip")
            .body("剪贴板历史为空")
            .show();
        return Ok(());
    }

    // 生成 fzf 输入：★\t{id}\t{preview}
    let mut input = String::new();
    for c in &clips {
        let star = if c.pinned { "★" } else { " " };
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        // fzf 需要 tab 分隔，注意 preview 中的 tab/换行已在 preview_text 处理
        input.push_str(&format!("{}\t{}\t{}\n", star, c.id, preview));
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
    let pin_cmd = format!("{} pin {{2}}", exe);
    let del_cmd = format!("{} delete {{2}}", exe);
    let wipe_cmd = format!("{} wipe", exe);

    let preview_cmd = if cfg.enable_preview {
        format!("{} preview {{2}}", exe)
    } else {
        "echo {3..}".to_string()
    };

    let mut fzf = Command::new("fzf")
        .arg("--no-sort")
        .arg("--delimiter=\t")
        .arg("--with-nth=1,3..")
        .arg("--tabstop=1")
        .arg("--height=100%")
        .arg("--layout=reverse")
        .arg("--border")
        .arg("--info=inline")
        .arg("--prompt=剪贴板> ")
        .arg("--header=Enter粘贴 · ^P固定 · ^X删除 · Alt-X清空 · ^R刷新")
        .arg("--track")
        .arg("--id-nth=2")
        .arg(format!("--preview={}", preview_cmd))
        .arg("--preview-window=down:5:wrap:border-rounded")
        .arg(format!("--bind=ctrl-p:execute-silent({})+reload-sync({})", pin_cmd, reload_cmd))
        .arg(format!("--bind=ctrl-x:execute-silent({})+reload-sync({})", del_cmd, reload_cmd))
        .arg(format!("--bind=ctrl-r:reload-sync({})", reload_cmd))
        .arg(format!("--bind=alt-x:execute({})+reload-sync({})", wipe_cmd, reload_cmd))
        .arg("--bind=ctrl-f:accept")
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
    if parts.len() < 2 {
        return Ok(());
    }
    let id: i64 = parts[1].parse().unwrap_or(0);
    if id == 0 {
        return Ok(());
    }
    let clip = store::get(id)?;
    // wl-copy
    let mut wl = Command::new("wl-copy").stdin(Stdio::piped()).spawn().context("wl-copy")?;
    wl.stdin.as_mut().unwrap().write_all(clip.text.as_bytes())?;
    wl.wait()?;
    println!("pasted {}", id);
    Ok(())
}

fn run_fuzzel(cfg: &Config) -> Result<()> {
    let clips = store::list(cfg.max_items)?;
    if clips.is_empty() {
        return Ok(());
    }
    let mut input = String::new();
    for c in &clips {
        let star = if c.pinned { "★ " } else { "" };
        let preview = crate::preview::preview_text(c, cfg.preview_width);
        input.push_str(&format!("{}{} {}\n", star, c.id, preview));
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
    let id_str = sel.trim_start_matches('★').trim().split_whitespace().next().unwrap_or("0");
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
    let clips = store::list(cfg.max_items)?;
    for c in clips {
        let star = if c.pinned { "★" } else { " " };
        let preview = crate::preview::preview_text(&c, cfg.preview_width);
        println!("{}\t{}\t{}", star, c.id, preview);
    }
    Ok(())
}

pub fn preview_id(id: i64) -> Result<()> {
    let c = store::get(id)?;
    // 输出全量文本，截断 2000 字符 + 100 行
    for line in c.text.lines().take(100) {
        let l: String = line.chars().take(300).collect();
        println!("{}", l);
    }
    if c.text.len() > 2000 {
        println!("… ({} bytes)", c.text.len());
    }
    Ok(())
}
