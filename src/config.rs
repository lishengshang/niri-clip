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
}

fn default_max_items() -> usize { 750 }
fn default_preview_width() -> usize { 100 }
fn default_min_store() -> usize { 1 }
fn default_regex() -> String { r"(?i)password|secret|token|otp|auth".to_string() }
fn default_true() -> bool { true }
fn default_backend() -> String { "auto".to_string() }

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
            .unwrap_or_else(|| PathBuf::from("/home/mio/.config"))
            .join("niri-clip/config.toml")
    }

    pub fn db_path() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("niri-clip/db.sqlite")
    }

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
