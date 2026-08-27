use crate::store::Clip;

pub fn is_image_clip(clip: &Clip) -> bool {
    clip.mime.starts_with("image/")
}

/// 生成预览文本：优先全量，截断到 preview_width
pub fn preview_text(clip: &Clip, width: usize) -> String {
    let mut s = clip.text.replace('\n', " ↵ ");
    if s.chars().count() > width {
        s = s.chars().take(width).collect::<String>() + "…";
    }
    s
}

/// 检查是否有 kitty icat / chafa 可用
pub fn image_preview_available() -> bool {
    which::which("chafa").is_ok() || which::which("kitty").is_ok()
}

pub fn preview_for_fzf(clip: &Clip, cfg: &crate::config::Config) -> String {
    if cfg.enable_image_preview && is_image_clip(clip) {
        if which::which("chafa").is_ok() {
            return format!("chafa preview for id {}", clip.id);
        }
        if which::which("kitty").is_ok() {
            return format!("kitty icat preview for id {}", clip.id);
        }
    }
    preview_text(clip, cfg.preview_width)
}

/// 实际渲染：供 tui preview 调用，输出到 stdout
pub fn render_preview(clip: &Clip) -> String {
    if clip.mime.starts_with("image/") {
        // 尝试从缓存目录找最新图片
        let cache = dirs::cache_dir().unwrap().join("niri-clip/images");
        if let Ok(entries) = std::fs::read_dir(&cache) {
            let mut files: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            files.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH));
            if let Some(last) = files.last() {
                let path = last.path();
                if which::which("chafa").is_ok() {
                    if let Ok(out) = std::process::Command::new("chafa")
                        .args(["--format", "symbols", "--size", "60x20", &path.to_string_lossy().to_string()])
                        .output()
                    {
                        if out.status.success() {
                            return String::from_utf8_lossy(&out.stdout).to_string();
                        }
                    }
                }
                if which::which("kitty").is_ok() {
                    // kitty icat 需在 kitty 终端内，直接返回提示
                    return format!("[image {} - kitty icat: {}]", clip.mime, path.display());
                }
            }
        }
        return format!("[{} {} bytes - preview requires chafa]", clip.mime, clip.text.len());
    }
    String::new()
}
