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
    let Some(path): Option<PathBuf> = clip.image_path.clone().map(PathBuf::from) else {
        return "[{} 该图片条目未记录数据文件]".replace("{}", &clip.mime);
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
