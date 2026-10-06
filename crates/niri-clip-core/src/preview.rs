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

/// 单字符显示列宽（pragmatic 口径）：East Asian Wide/Fullwidth ≈ 2，其余 1，
/// 控制字符 0。覆盖 CJK/全角/emoji 主区段；组合附加符号与零宽字符按 1 计
/// （预览行不做规范化合并，一格误差可接受）。仅供 `preview_text_columns`。
fn display_width(c: char) -> usize {
    let cp = c as u32;
    if cp < 0x20 || (0x7f..=0x9f).contains(&cp) {
        return 0;
    }
    if (0x1100..=0x115f).contains(&cp) // Hangul Jamo
        || (0x2e80..=0x303e).contains(&cp) // CJK 部首/符号
        || (0x3041..=0x33ff).contains(&cp) // 假名/注音/CJK 符号
        || (0x3400..=0x4dbf).contains(&cp) // CJK 扩展 A
        || (0x4e00..=0x9fff).contains(&cp) // CJK 基本区
        || (0xa000..=0xa4cf).contains(&cp) // 彝文
        || (0xac00..=0xd7a3).contains(&cp) // Hangul 音节
        || (0xf900..=0xfaff).contains(&cp) // CJK 兼容表意
        || (0xfe30..=0xfe4f).contains(&cp) // CJK 兼容形式
        || (0xff00..=0xff60).contains(&cp) // 全角 ASCII/形式
        || (0xffe0..=0xffe6).contains(&cp) // 全角符号
        || (0x1f300..=0x1faff).contains(&cp) // emoji 主体区
        || (0x20000..=0x3fffd).contains(&cp)
    // CJK 扩展 B 及以后
    {
        2
    } else {
        1
    }
}

/// 按显示列宽截断的预览行——GUI 列表专用：窗口是像素世界，CJK/emoji 的
/// advance 约为拉丁 2 倍，`preview_text` 的"字符数"预算在混排文本下会让
/// iced 在行尾像素级硬裁（省略号被裁掉、字切半）。本函数按"列"累计
/// （拉丁 1 / 宽字符 2），截断必补 `…`，预算由调用方按窗口像素换算。
/// TUI 走终端字符网格，继续用 `preview_text`，语义不变。
pub fn preview_text_columns(clip: &Clip, cols: usize) -> String {
    if cols == 0 {
        return String::new();
    }
    // 省略号固定占 1 列；换行替换串 " ↵ " 占 3 列（与 preview_text 同语义）
    let budget = cols.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0usize;
    let mut truncated = false;
    for ch in clip.text.chars() {
        let (w, s): (usize, String) = if ch == '\n' {
            (3, " ↵ ".to_string())
        } else {
            (display_width(ch), ch.to_string())
        };
        if used + w > budget {
            truncated = true;
            break;
        }
        used += w;
        out.push_str(&s);
    }
    if truncated {
        out.push('…');
    }
    out
}

/// 多行预览结果
pub struct MultilinePreview {
    pub text: String,
    /// 是否发生截断（行数超限或某行被截断）——调用方据此决定提示文案
    pub truncated: bool,
}

/// CLI `preview <id>` 的规格：最多 100 行 × 每行 300 字符
pub const MULTILINE_CLI: (usize, usize) = (100, 300);
/// 原生 UI 底部预览窗格的规格：窗格定高 220px，80 行足够且省一次全量遍历
pub const MULTILINE_GUI: (usize, usize) = (80, 300);

/// 多行预览（逐行截断）——CLI `preview` 子命令与原生 UI 底部窗格共用的**唯一实现**。
///
/// 任务 2.6 / C5：此前两处各自实现，且 GUI 侧用 `text.len() > out.len()`（字节比较）
/// 判断是否截断，与"逐行按**字符**截断"的口径不匹配，存在漏判。现统一在 core。
///
/// 返回的 `text` 每行以 `\n` 结尾；`truncated` 覆盖两种情况：行数超限、某行被截断。
pub fn preview_multiline(clip: &Clip, max_lines: usize, line_width: usize) -> MultilinePreview {
    let mut out = String::new();
    let mut truncated = false;
    for (i, line) in clip.text.lines().enumerate() {
        if i >= max_lines {
            truncated = true;
            break;
        }
        let l: String = line.chars().take(line_width).collect();
        if l.chars().count() < line.chars().count() {
            truncated = true;
        }
        out.push_str(&l);
        out.push('\n');
    }
    MultilinePreview {
        text: out,
        truncated,
    }
}

/// 实际渲染：供 `tui preview <id>` 输出到 stdout。
///
/// v0.4 正确性修复：数据文件按 clip id 关联（images/{id}.bin，
/// 路径存于 clips.image_path）。旧实现"取 images 目录里 mtime 最新的一张"
/// 必然把最近一次复制的图渲染到所有图片条目上。
/// chafa 渲染尺寸（符号格式，`宽x高` 字符数）。与 `preview_width`（列表单行
/// 截断宽度）是不同概念：后者是 UI 行宽的字符数，这里是终端图形的字符网格，
/// 故独立常量并显式说明（任务 2.6 / D10）
const CHAFA_SIZE: &str = "60x20";

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
                CHAFA_SIZE,
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
    fn preview_text_columns_counts_cjk_as_two() {
        // 10 列预算（省略号占 1，剩 9）：4 个汉字占满 8 列，第 5 个放不下
        let c = text_clip("中文中文中文中");
        let out = preview_text_columns(&c, 10);
        assert_eq!(out, "中文中文…");
        // 纯拉丁同预算：9 字符 + 省略号
        let l = text_clip(&"x".repeat(200));
        let out = preview_text_columns(&l, 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn preview_text_columns_mixed_latin_cjk() {
        // budget=6：a b 中 c d 用满 6 列不截断；再收紧 1 列则 d 出局
        let c = text_clip("ab中cd");
        assert_eq!(preview_text_columns(&c, 7), "ab中cd");
        assert_eq!(preview_text_columns(&c, 6), "ab中c…");
    }

    #[test]
    fn preview_text_columns_no_truncation_within_budget() {
        // 恰好等于预算（省略号位不用）：不截断、无省略号
        let c = text_clip("ab中");
        assert_eq!(preview_text_columns(&c, 5), "ab中");
        assert_eq!(preview_text_columns(&c, 0), "", "零列返回空串");
    }

    #[test]
    fn preview_text_columns_flattens_newlines() {
        let c = text_clip("ab\ncd");
        // budget=9：a b ↵(3) c d = 7 列，未截断
        let out = preview_text_columns(&c, 10);
        assert_eq!(out, "ab ↵ cd");
    }

    #[test]
    fn preview_text_columns_emoji_counts_as_two() {
        // budget=4：两个 emoji 占满，第三个放不下
        let c = text_clip("😀😀😀");
        assert_eq!(preview_text_columns(&c, 5), "😀😀…");
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
