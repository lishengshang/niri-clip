use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_max_items")]
    pub max_items: usize,
    #[serde(default = "default_preview_width")]
    pub preview_width: usize,
    #[serde(default = "default_min_store")]
    pub min_store_length: usize,
    #[serde(default)]
    pub enable_image_preview: bool,
    #[serde(default = "default_regex")]
    pub ignore_regex: String,
    #[serde(default = "default_true")]
    pub pinned_on_top: bool,
    /// auto | fzf | fuzzel
    #[serde(default = "default_backend")]
    pub tui_backend: String,
    #[serde(default = "default_true")]
    pub enable_preview: bool,
    /// v0.4.1：事件模式下每次捕获子进程的超时秒数（防止病态读挂起长期占用）
    #[serde(default = "default_capture_timeout")]
    pub capture_timeout_secs: u64,
    /// v0.5：单条文本入库上限（字节），超限拒绝入库，防 DB 膨胀与全内存直通
    #[serde(default = "default_max_clip_bytes")]
    pub max_clip_bytes: usize,
    /// v0.5：单张图片入库上限（字节），截图通常 1–3MB，给足余量
    #[serde(default = "default_max_image_bytes")]
    pub max_image_bytes: usize,
}

fn default_max_items() -> usize {
    750
}
fn default_preview_width() -> usize {
    100
}
fn default_min_store() -> usize {
    1
}
fn default_regex() -> String {
    r"(?i)password|secret|token|otp|auth".to_string()
}
fn default_true() -> bool {
    true
}
fn default_backend() -> String {
    "auto".to_string()
}
fn default_capture_timeout() -> u64 {
    5
}
fn default_max_clip_bytes() -> usize {
    1_048_576 // 1 MiB
}
fn default_max_image_bytes() -> usize {
    10_485_760 // 10 MiB
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_items: 750,
            preview_width: 100,
            min_store_length: 1,
            enable_image_preview: false,
            ignore_regex: r"(?i)password|secret|token|otp|auth".to_string(),
            pinned_on_top: true,
            tui_backend: "auto".to_string(),
            enable_preview: true,
            capture_timeout_secs: 5,
            max_clip_bytes: 1_048_576,
            max_image_bytes: 10_485_760,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(s) => match toml::from_str::<Config>(&s) {
                    Ok(c) => return c,
                    Err(e) => eprintln!("[niri-clip] config parse error {e}, using default"),
                },
                Err(e) => eprintln!("[niri-clip] read config error {e}"),
            }
        }
        Self::default()
    }

    pub fn path() -> PathBuf {
        dirs::config_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
            .expect("cannot determine XDG_CONFIG_HOME / HOME")
            .join("niri-clip/config.toml")
    }

    /// v0.4：状态目录。剪贴板历史属于应持久保存的用户状态（XDG state 规范），
    /// 不再放 ~/.cache——那会被系统清理工具当作可回收缓存误删。
    pub fn state_dir() -> PathBuf {
        if let Ok(v) = std::env::var("XDG_STATE_HOME") {
            let p = PathBuf::from(v);
            if p.is_absolute() {
                return p.join("niri-clip");
            }
        }
        if let Some(h) = dirs::home_dir() {
            return h.join(".local/state/niri-clip");
        }
        std::env::temp_dir().join("niri-clip")
    }

    /// v0.4：图片数据文件目录（与库同级随行，内容不可再生所以必须在 state）
    pub fn images_dir() -> PathBuf {
        Self::state_dir().join("images")
    }

    pub fn db_path() -> PathBuf {
        Self::state_dir().join("db.sqlite")
    }

    /// v0.3 及之前使用的旧位置；connect 时自动快照搬迁到 db_path
    pub fn legacy_db_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("niri-clip/db.sqlite")
    }

    #[allow(dead_code)]
    pub fn legacy_cliphist_db() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("cliphist/db")
    }

    pub fn ensure_dirs() -> Result<()> {
        if let Some(p) = Self::path().parent() {
            std::fs::create_dir_all(p)?;
        }
        if let Some(p) = Self::db_path().parent() {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }
}
