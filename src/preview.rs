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
    }
    preview_text(clip, cfg.preview_width)
}
