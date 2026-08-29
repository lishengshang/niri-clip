use crate::store::Clip;
use std::path::{Path, PathBuf};

/// 生成预览行文本：单行化 + 截断到 width。
///
/// v0.4 性能修复：此前先对全文 `replace('\n', ...)` 再截断——大文本条目在每次
/// fzf reload-sync（300 行 × 每条全量扫描）时成为真实热点。现改为先廉价取
/// `width+1` 个字符判断溢出（O(width)，不再整串计数），仅对小窗口做替换。
pub fn preview_text(clip: &Clip, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let head: String = clip.text.chars().take(width).collect();
    let over = clip.text.chars().nth(width).is_some();
    let marked = head.replace('\n', " ↵ ");
    if over {
        // 换行替换可能拉长展示串，回收回宽度后再补省略号
        let mut out: String = marked.chars().take(width).collect();
        out.push('…');
        return out;
    }
    marked
}

/// 实际渲染：供 `tui preview <id>` 输出到 stdout。
///
/// v0.4 正确性修复：数据文件按 clip id 关联（images/{id}.bin，
/// 路径存于 clips.image_path）。旧实现"取 images 目录里 mtime 最新的一张"
/// 必然把最近一次复制的图渲染到所有图片条目上。
pub fn render_preview(clip: &Clip) -> String {
    if !clip.mime.starts_with("image/") {
        return String::new();
    }
    let Some(path) = clip.image_path.as_deref().map(PathBuf::from) else {
        return format!("[{} 该图片条目未记录数据文件]", clip.mime);
    };
    let path = Path::new(&path);
    if !path.exists() {
        return format!("[图片数据文件丢失: {}]", path.display());
    }
    if which::which("chafa").is_ok() {
        if let Ok(out) = std::process::Command::new("chafa")
            .args([
                "--format",
                "symbols",
                "--size",
                "60x20",
                &path.to_string_lossy(),
            ])
            .output()
        {
            if out.status.success() {
                return String::from_utf8_lossy(&out.stdout).to_string();
            }
        }
    }
    if which::which("kitty").is_ok() {
        // kitty icat 需在 kitty 终端内运行，此处给出路径供用户直接调用
        return format!("[image {} - kitty icat: {}]", clip.mime, path.display());
    }
    format!("[{} - 预览需要安装 chafa]", clip.mime)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_clip(s: &str) -> Clip {
        Clip {
            id: 1,
            hash: String::new(),
            text: s.to_string(),
            mime: "text/plain".to_string(),
            pinned: false,
            image_path: None,
        }
    }

    #[test]
    fn preview_text_truncates_to_width_with_ellipsis() {
        let c = text_clip(&"x".repeat(200));
        let out = preview_text(&c, 10);
        assert_eq!(out.chars().count(), 11, "10 字符 + 省略号");
        assert!(out.ends_with('…'));
    }

    #[test]
    fn preview_text_flattens_newlines() {
        let c = text_clip("line1\nline2\nline3");
        let out = preview_text(&c, 100);
        assert!(out.contains("line1 ↵ line2 ↵ line3"), "换行应单行化: {out}");
        assert_eq!(out.lines().count(), 1);
    }

    #[test]
    fn preview_text_short_input_passes_through() {
        let c = text_clip("hello");
        assert_eq!(preview_text(&c, 100), "hello");
        // 恰好等于宽度：不溢出、无省略号
        assert_eq!(preview_text(&c, 5), "hello");
        assert_eq!(preview_text(&c, 0), "", "零宽度返回空串");
    }

    #[test]
    fn preview_text_multibyte_truncation_is_char_aligned() {
        // 中文字符占 3 字节：截断必须按字符而非字节，否则拼出半个字符
        let c = text_clip("你好世界你好世界");
        let out = preview_text(&c, 4);
        assert_eq!(out.chars().count(), 5, "4 个汉字 + 省略号");
        assert!(out.starts_with("你好世界"));
    }

    #[test]
    fn render_preview_text_clip_is_empty() {
        // 文本条目不走图片渲染链路（tui preview 只对 mime=image/* 调用，
        // 这里锁定防御性行为：文本返回空串）
        let c = text_clip("plain");
        assert_eq!(render_preview(&c), "");
    }

    #[test]
    fn render_preview_image_without_path_or_file_degrades() {
        let mut c = text_clip("[image placeholder]");
        c.mime = "image/png".to_string();
        // 无 image_path：给出占位说明而非 panic
        assert!(render_preview(&c).contains("未记录数据文件"));
        // 有路径但文件缺失
        c.image_path = Some("/nonexistent/niri-clip-ut/42.bin".to_string());
        assert!(render_preview(&c).contains("丢失"));
    }
}
