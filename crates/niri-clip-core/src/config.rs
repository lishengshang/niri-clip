use anyhow::Result;
use regex::Regex;
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
    /// v0.5（P1-4）：桌面通知开关（mako 等）。关闭后完全静默运行
    #[serde(default = "default_true")]
    pub notify_enabled: bool,
    /// v0.5.1（1.3）：images/ 目录总量配额（字节），超出后 daemon 启动时按
    /// LRU 淘汰最旧图片条目（星标与当前项受保护）；0 = 不限制
    #[serde(default = "default_max_image_total_bytes")]
    pub max_image_total_bytes: usize,
    /// v0.5.2（1.1）：PRIMARY selection 捕获开关。开启后鼠标划选的文本
    /// 也入库（中键粘贴语义）；与剪贴板同去重空间。默认关——划选噪声大
    #[serde(default)]
    pub capture_primary: bool,
    /// ignore_regex 的编译产物：每条入库热路径都要过一遍过滤，不能在
    /// should_ignore 里反复 Regex::new。编译失败为 None（同旧行为：不过滤）。
    /// 注意：直接改字段赋值 ignore_regex 时需同步重编译（代码内无此用法）
    #[serde(skip)]
    #[serde(default)]
    pub ignore_re: Option<Regex>,
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
fn default_max_image_total_bytes() -> usize {
    209_715_200 // 200 MiB
}

impl Default for Config {
    fn default() -> Self {
        let ignore_regex = r"(?i)password|secret|token|otp|auth".to_string();
        Self {
            max_items: 750,
            preview_width: 100,
            min_store_length: 1,
            enable_image_preview: false,
            ignore_re: Regex::new(&ignore_regex).ok(),
            ignore_regex,
            pinned_on_top: true,
            tui_backend: "auto".to_string(),
            enable_preview: true,
            capture_timeout_secs: 5,
            max_clip_bytes: 1_048_576,
            max_image_bytes: 10_485_760,
            max_image_total_bytes: 209_715_200,
            capture_primary: false,
            notify_enabled: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(s) => match toml::from_str::<Config>(&s) {
                    Ok(mut c) => {
                        c.ignore_re = Regex::new(&c.ignore_regex).ok();
                        return c;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 store/tui 测试共享全局 XDG 锁（见 lib.rs test_util 注释）
    use crate::test_util::ENV_LOCK;

    #[test]
    fn default_config_compiles_ignore_regex() {
        let cfg = Config::default();
        assert_eq!(cfg.max_items, 750);
        assert_eq!(cfg.max_clip_bytes, 1_048_576);
        assert_eq!(cfg.max_image_bytes, 10_485_760);
        assert!(!cfg.enable_image_preview, "图片捕获默认关");
        assert!(!cfg.capture_primary, "PRIMARY 捕获默认关");
        assert_eq!(cfg.tui_backend, "auto");
        // 编译产物就位：should_ignore 不必再 Regex::new
        assert!(cfg.ignore_re.is_some(), "默认正则必须可编译");
        assert!(cfg.ignore_re.as_ref().unwrap().is_match("my PASSWORD"));
        assert!(!cfg.ignore_re.as_ref().unwrap().is_match("hello world"));
    }

    #[test]
    fn load_parses_toml_and_compiles_custom_regex() {
        let _g = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("niri-clip-cfg-{}", std::process::id()));
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(cfg_dir.join("niri-clip")).unwrap();
        std::fs::write(
            cfg_dir.join("niri-clip/config.toml"),
            "max_items = 42\nignore_regex = \"topsecret\"\n",
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        let cfg = Config::load();
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(cfg.max_items, 42);
        // 未写的字段回落默认值
        assert_eq!(cfg.preview_width, 100);
        // 自定义正则已编译且生效
        assert!(cfg.ignore_re.is_some());
        assert!(cfg.ignore_re.as_ref().unwrap().is_match("topsecret-data"));
        assert!(!cfg.ignore_re.as_ref().unwrap().is_match("password"));
    }

    #[test]
    fn load_invalid_toml_falls_back_to_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("niri-clip-cfg-bad-{}", std::process::id()));
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(cfg_dir.join("niri-clip")).unwrap();
        std::fs::write(
            cfg_dir.join("niri-clip/config.toml"),
            "max_items = [broken\n",
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        let cfg = Config::load();
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(cfg.max_items, 750, "解析失败应回退默认值");
    }

    #[test]
    fn load_invalid_regex_means_no_filter() {
        let _g = ENV_LOCK.lock().unwrap();
        let root = std::env::temp_dir().join(format!("niri-clip-cfg-re-{}", std::process::id()));
        let cfg_dir = root.join("config");
        std::fs::create_dir_all(cfg_dir.join("niri-clip")).unwrap();
        // 非法正则：编译失败 → ignore_re=None（过滤关闭），而不是整个配置崩掉
        std::fs::write(
            cfg_dir.join("niri-clip/config.toml"),
            "ignore_regex = \"([unclosed\"\n",
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
        let cfg = Config::load();
        std::env::remove_var("XDG_CONFIG_HOME");
        let _ = std::fs::remove_dir_all(&root);

        assert!(cfg.ignore_re.is_none(), "非法正则应得 None");
        assert!(!crate::store::should_ignore("password", &cfg));
    }

    #[test]
    fn state_dir_respects_xdg_env() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("XDG_STATE_HOME").ok();
        std::env::set_var("XDG_STATE_HOME", "/tmp/nc-ut-state");
        assert_eq!(
            Config::state_dir(),
            PathBuf::from("/tmp/nc-ut-state/niri-clip")
        );
        assert_eq!(
            Config::db_path(),
            PathBuf::from("/tmp/nc-ut-state/niri-clip/db.sqlite")
        );
        // 相对路径不被接受，回落 HOME
        std::env::set_var("XDG_STATE_HOME", "relative/path");
        let fallback = Config::state_dir();
        assert!(
            !fallback.starts_with("relative"),
            "相对路径必须被拒绝: {:?}",
            fallback
        );
        match prev {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }
}
