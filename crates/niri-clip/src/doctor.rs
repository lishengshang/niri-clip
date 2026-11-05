//! `niri-clip doctor`：v1.0 接入自检（ROADMAP 4.9）。
//!
//! 只读探测系统状态：配置/数据库、daemon flock、systemd 单元与自启、
//! niri spawn-at-startup 与快捷键、TUI 后端与运行依赖。唯一的副作用是
//! `InstanceGuard::try_acquire` 会创建 state 目录与 `daemon.lock`——与
//! daemon 启动路径一致，用于判断锁是否被占用。
//!
//! 输出约定：`[ok]` / `[info]` / `[warn]` / `[fail]`；`fail` 使退出码非零，
//! `warn` 不影响退出码，方便安装脚本与 CI 消费。

use anyhow::Result;
use niri_clip_core::config::Config;
use niri_clip_core::single_instance::InstanceGuard;
use niri_clip_core::store;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::tui;

macro_rules! outln {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($arg)*);
    }};
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Check {
    level: Level,
    title: String,
    detail: String,
    hint: Option<String>,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    fn add(&mut self, level: Level, title: &str, detail: impl Into<String>, hint: Option<String>) {
        self.checks.push(Check {
            level,
            title: title.to_string(),
            detail: detail.into(),
            hint,
        });
    }

    fn ok(&mut self, title: &str, detail: impl Into<String>) {
        self.add(Level::Ok, title, detail, None);
    }

    fn info(&mut self, title: &str, detail: impl Into<String>) {
        self.add(Level::Info, title, detail, None);
    }

    fn warn(&mut self, title: &str, detail: impl Into<String>, hint: Option<&str>) {
        self.add(Level::Warn, title, detail, hint.map(|s| s.to_string()));
    }

    fn fail(&mut self, title: &str, detail: impl Into<String>, hint: Option<&str>) {
        self.add(Level::Fail, title, detail, hint.map(|s| s.to_string()));
    }

    fn count(&self, level: Level) -> usize {
        self.checks.iter().filter(|c| c.level == level).count()
    }

    fn failures(&self) -> usize {
        self.count(Level::Fail)
    }

    fn print(&self) {
        outln!(
            "niri-clip doctor — 环境自检（v{}）",
            env!("CARGO_PKG_VERSION")
        );
        outln!("配置：{}", Config::path().display());
        outln!("状态：{}", Config::state_dir().display());
        outln!();
        for c in &self.checks {
            let tag = match c.level {
                Level::Ok => "ok",
                Level::Info => "info",
                Level::Warn => "warn",
                Level::Fail => "fail",
            };
            outln!("[{tag}] {}：{}", c.title, c.detail);
            if let Some(hint) = &c.hint {
                outln!("       → {hint}");
            }
        }
        outln!();
        outln!(
            "汇总：{} 通过 · {} 提醒 · {} 失败",
            self.count(Level::Ok),
            self.count(Level::Warn),
            self.failures()
        );
    }
}

pub fn run() -> Result<()> {
    let (cfg, config_error) = match Config::load_checked() {
        Ok(c) => (c, None),
        Err(e) => (Config::default(), Some(format!("{e:#}"))),
    };

    let mut report = Report::default();
    check_config(&mut report, &cfg, config_error.as_deref());
    check_state_and_db(&mut report);
    let systemd = check_systemd(&mut report);
    let niri = check_niri(&mut report);
    let daemon_running = check_daemon(&mut report);
    check_autostart(&mut report, daemon_running, &systemd, &niri);
    check_backend(&mut report, &cfg);
    check_runtime_deps(&mut report, &cfg);

    report.print();

    if report.failures() > 0 {
        anyhow::bail!(
            "doctor 发现 {} 项失败，请按上方提示修复后重试",
            report.failures()
        );
    }
    Ok(())
}

fn check_config(report: &mut Report, cfg: &Config, error: Option<&str>) {
    let path = Config::path();
    if let Some(error) = error {
        report.fail(
            "配置",
            format!("{} 解析失败：{error}", path.display()),
            Some("修复 TOML 后重试；当前已回退内置默认值"),
        );
    } else if path.exists() {
        report.ok("配置", path.display().to_string());
    } else {
        report.info("配置", format!("{} 不存在，使用内置默认值", path.display()));
    }

    if cfg.ignore_re.is_none() {
        report.fail(
            "敏感过滤",
            "ignore_regex 无法编译，命中规则不会生效",
            Some("修正 config.toml 的 ignore_regex（TOML 建议用单引号字面量）"),
        );
    } else {
        report.ok("敏感过滤", "ignore_regex 可编译");
    }

    if cfg.max_items == 0 {
        report.fail(
            "max_items",
            "为 0，历史不会保留",
            Some("设为 ≥1（默认 750）"),
        );
    }
    if cfg.max_clip_bytes == 0 {
        report.fail(
            "max_clip_bytes",
            "为 0，任何非空文本都会被拒绝",
            Some("设为 ≥1（默认 1048576）"),
        );
    }
    if cfg.capture_timeout_secs == 0 {
        report.fail(
            "capture_timeout_secs",
            "为 0，捕获子进程会被立即杀死",
            Some("设为 ≥1（默认 5）"),
        );
    }
    if cfg.max_image_bytes == 0 && cfg.enable_image_preview {
        report.fail(
            "max_image_bytes",
            "为 0 但图片捕获已开启",
            Some("设为 ≥1（默认 10485760）"),
        );
    }
    if !matches!(
        cfg.tui_backend.as_str(),
        "auto" | "native" | "fzf" | "fuzzel"
    ) {
        report.warn(
            "tui_backend",
            format!("未知值 {:?}，将按 auto 处理", cfg.tui_backend),
            Some("改为 auto|native|fzf|fuzzel"),
        );
    }
}

fn check_state_and_db(report: &mut Report) {
    let state = Config::state_dir();
    let db = Config::db_path();

    if !state.exists() {
        report.info(
            "state 目录",
            format!("{} 尚未创建（daemon 首次启动时创建）", state.display()),
        );
    }

    if db.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&state) {
                let mode = meta.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    report.warn(
                        "state 权限",
                        format!("{} 为 {mode:03o}（期望 0700）", state.display()),
                        Some("连接数据库时 core 会自动收紧；若持续异常请检查目录属主"),
                    );
                } else {
                    report.ok("state 权限", format!("{mode:03o}"));
                }
            }
        }
        match store::stats() {
            Ok(s) => {
                let total = s.db_bytes + s.images_disk_bytes;
                report.ok(
                    "数据库",
                    format!(
                        "{} 条，库 {} + 图片 {} = {}",
                        s.total,
                        crate::fmt_bytes(s.db_bytes),
                        crate::fmt_bytes(s.images_disk_bytes),
                        crate::fmt_bytes(total)
                    ),
                );
            }
            Err(e) => report.fail(
                "数据库",
                format!("读取 {} 失败：{e:#}", db.display()),
                Some("检查 state 目录权限与磁盘空间"),
            ),
        }
    } else {
        report.info(
            "数据库",
            format!("{} 尚未创建（首次捕获时自动创建）", db.display()),
        );
        let legacy = Config::legacy_db_path();
        if legacy.exists() {
            report.info(
                "旧库",
                format!("检测到 {}，首次连接会自动快照迁移", legacy.display()),
            );
        }
    }
}

#[derive(Default)]
struct SystemdFacts {
    enabled: bool,
    unit_installed: bool,
}

fn systemctl(args: &[&str]) -> Option<String> {
    let out = Command::new("systemctl").args(args).output().ok()?;
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).trim().to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    if text.is_empty() && !out.status.success() {
        return None;
    }
    Some(text)
}

fn parse_systemd_version(s: &str) -> Option<u32> {
    s.split_whitespace().find_map(|w| w.parse::<u32>().ok())
}

fn unit_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(config) = dirs::config_dir() {
        paths.push(config.join("systemd/user/niri-clip.service"));
    }
    paths.push(PathBuf::from("/usr/lib/systemd/user/niri-clip.service"));
    paths.push(PathBuf::from("/etc/systemd/user/niri-clip.service"));
    paths
}

fn check_systemd(report: &mut Report) -> SystemdFacts {
    let mut facts = SystemdFacts::default();

    let Some(active) = systemctl(&["--user", "is-active", "niri-clip.service"]) else {
        report.info(
            "systemd",
            "systemctl 不可用（非 systemd 环境），跳过单元检查",
        );
        return facts;
    };
    if active.contains("Failed to connect") {
        report.info(
            "systemd",
            format!("用户总线不可达（{active}），跳过单元检查"),
        );
        return facts;
    }

    match active.as_str() {
        "active" => report.ok("systemd", "niri-clip.service active"),
        "inactive" => report.info("systemd", "niri-clip.service inactive（未运行）"),
        "failed" => report.warn(
            "systemd",
            "niri-clip.service failed",
            Some("journalctl --user -u niri-clip -n 50"),
        ),
        other => report.info("systemd", format!("niri-clip.service: {other}")),
    }

    let enabled = systemctl(&["--user", "is-enabled", "niri-clip.service"]).unwrap_or_default();
    facts.enabled = enabled == "enabled" || enabled == "enabled-runtime";
    match enabled.as_str() {
        "enabled" | "enabled-runtime" => report.ok("systemd 自启", enabled),
        "disabled" => report.info("systemd 自启", "未 enable（若用 niri spawn 启动可忽略）"),
        "not-found" => report.info("systemd 单元", "未安装（可用 niri-clip install-service）"),
        other if !other.is_empty() => report.info("systemd 自启", other),
        _ => report.info("systemd 自启", "状态未知"),
    }

    let unit = unit_paths().into_iter().find(|p| p.exists());
    if let Some(path) = unit {
        facts.unit_installed = true;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let sandboxed = text.contains("RestrictAddressFamilies=AF_UNIX")
                    && text.contains("StateDirectory=niri-clip");
                if sandboxed {
                    report.ok(
                        "systemd 沙箱",
                        format!("已包含 3.5 指令（{}）", path.display()),
                    );
                } else {
                    report.warn(
                        "systemd 沙箱",
                        format!("{} 缺少 3.5 沙箱指令", path.display()),
                        Some("同步/重装 niri-clip.service 后 systemctl --user daemon-reload"),
                    );
                }
            }
            Err(e) => report.warn(
                "systemd 单元",
                format!("读取 {} 失败：{e}", path.display()),
                None,
            ),
        }
    } else {
        report.info(
            "systemd 单元",
            "未在 ~/.config 或 /usr/lib 找到 niri-clip.service",
        );
    }

    if facts.unit_installed {
        if let Some(version) = systemctl(&["--version"]).and_then(|s| parse_systemd_version(&s)) {
            if version < 248 {
                report.warn(
                    "systemd 版本",
                    format!("{version} < 248，StateDirectory= 不可用"),
                    Some("升级 systemd，或改用 niri spawn-at-startup"),
                );
            } else {
                report.ok("systemd 版本", format!("{version}（≥248）"));
            }
        }
    }

    facts
}

#[derive(Debug, Default, PartialEq, Eq)]
struct NiriFacts {
    spawn_valid: bool,
    spawn_invalid_single: bool,
    spawn_without_daemon: bool,
    bind_found: bool,
    bind_key: Option<String>,
    includes: Vec<String>,
}

fn niri_config_path() -> PathBuf {
    if let Some(path) = std::env::var_os("NIRI_CONFIG") {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path;
        }
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("niri/config.kdl")
}

fn quoted_strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else {
            break;
        };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
    out
}

fn key_from_header(header: &str) -> Option<String> {
    let head = header.split('{').next()?.trim();
    let first = head.split_whitespace().next()?;
    (first.contains('+') && !first.eq_ignore_ascii_case("binds")).then(|| first.to_string())
}

fn key_before_spawn(line: &str) -> Option<String> {
    let spawn = line.find("spawn")?;
    let before = &line[..spawn];
    let open = before.rfind('{')?;
    let start = before[..open].rfind(['{', '}']).map(|i| i + 1).unwrap_or(0);
    key_from_header(&before[start..=open])
}

fn find_bind_key(lines: &[&str], idx: usize) -> Option<String> {
    if let Some(key) = key_before_spawn(lines[idx]) {
        return Some(key);
    }
    for line in lines[..idx].iter().rev() {
        if let Some(key) = key_from_header(line) {
            return Some(key);
        }
        if line.contains('}') {
            break;
        }
    }
    None
}

fn parse_niri_config(text: &str) -> NiriFacts {
    let mut facts = NiriFacts::default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .collect();

    for line in &lines {
        if line.starts_with("include") {
            facts.includes.extend(quoted_strings(line));
        }
        if !line.starts_with("spawn-at-startup") {
            continue;
        }
        let tokens = quoted_strings(line);
        if tokens.len() == 1 && tokens[0].trim() == "niri-clip daemon" {
            facts.spawn_invalid_single = true;
            continue;
        }
        let has_bin = tokens.iter().any(|token| {
            token
                .split_whitespace()
                .any(|word| word == "niri-clip" || word.ends_with("/niri-clip"))
        });
        let has_daemon = tokens
            .iter()
            .any(|token| token.split_whitespace().any(|word| word == "daemon"));
        if has_bin && has_daemon {
            facts.spawn_valid = true;
        } else if has_bin {
            facts.spawn_without_daemon = true;
        }
    }

    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with("spawn-at-startup") {
            continue;
        }
        if !line.split_whitespace().any(|word| word == "spawn") {
            continue;
        }
        let tokens = quoted_strings(line);
        let has_bin = tokens
            .iter()
            .any(|token| token == "niri-clip" || token.ends_with("/niri-clip"));
        if !has_bin || tokens.iter().any(|token| token == "daemon") {
            continue;
        }
        facts.bind_found = true;
        if facts.bind_key.is_none() {
            facts.bind_key = find_bind_key(&lines, idx);
        }
    }

    facts
}

fn resolve_include(config_path: &Path, include: &str) -> Option<PathBuf> {
    if include.chars().any(|c| matches!(c, '*' | '?' | '[')) {
        return None;
    }
    let include_path = if let Some(rest) = include.strip_prefix("~/") {
        dirs::home_dir()?.join(rest)
    } else {
        PathBuf::from(include)
    };
    if include_path.is_absolute() {
        Some(include_path)
    } else {
        config_path.parent().map(|parent| parent.join(include_path))
    }
}

fn check_niri(report: &mut Report) -> NiriFacts {
    let path = niri_config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            report.info(
                "niri 配置",
                format!("未找到 {}（使用 systemd 时可忽略）", path.display()),
            );
            return NiriFacts::default();
        }
        Err(e) => {
            report.warn(
                "niri 配置",
                format!("读取 {} 失败：{e}", path.display()),
                None,
            );
            return NiriFacts::default();
        }
    };

    let main = parse_niri_config(&text);
    let mut combined = text;
    let mut expanded = 0usize;
    let mut skipped = Vec::new();
    for include in &main.includes {
        match resolve_include(&path, include) {
            Some(include_path) if include_path.is_file() => {
                match std::fs::read_to_string(&include_path) {
                    Ok(included) => {
                        combined.push('\n');
                        combined.push_str(&included);
                        expanded += 1;
                    }
                    Err(_) => skipped.push(include.clone()),
                }
            }
            _ => skipped.push(include.clone()),
        }
    }
    let facts = parse_niri_config(&combined);

    report.info("niri 配置", path.display().to_string());
    if !main.includes.is_empty() {
        report.info(
            "niri include",
            format!(
                "主配置 include {} 个文件，已展开 {} 个",
                main.includes.len(),
                expanded
            ),
        );
        if !skipped.is_empty() {
            report.info(
                "niri include",
                format!(
                    "未展开（glob/不可读）：{}（绑定可能在其中）",
                    skipped.join("、")
                ),
            );
        }
    }

    if facts.spawn_invalid_single {
        report.warn(
            "niri 自启",
            "spawn-at-startup 使用了单字符串 \"niri-clip daemon\"",
            Some("改为两个参数：spawn-at-startup \"niri-clip\" \"daemon\""),
        );
    } else if facts.spawn_valid {
        report.ok(
            "niri 自启",
            "已配置 spawn-at-startup \"niri-clip\" \"daemon\"",
        );
    } else if facts.spawn_without_daemon {
        report.warn(
            "niri 自启",
            "spawn-at-startup 未带 daemon 参数",
            Some("改为 spawn-at-startup \"niri-clip\" \"daemon\""),
        );
    } else {
        report.info(
            "niri 自启",
            "未配置 spawn-at-startup（使用 systemd 时可忽略）",
        );
    }

    if let Some(key) = &facts.bind_key {
        if key.eq_ignore_ascii_case("Mod+V") {
            report.ok("快捷键", format!("{key} → niri-clip tui"));
        } else {
            report.warn(
                "快捷键",
                format!("已绑定 {key} → niri-clip tui（README 默认 Mod+V）"),
                None,
            );
        }
    } else if facts.bind_found {
        report.warn("快捷键", "找到 niri-clip tui 绑定但未能识别键位", None);
    } else {
        let hint = if main.includes.is_empty() {
            "在 binds { Mod+V { spawn \"niri-clip\" \"tui\"; } } 中添加"
        } else {
            "主配置与已展开的 include 文件中均未找到；若使用 glob/嵌套 include 请手工确认"
        };
        report.warn("快捷键", "未找到 niri-clip tui 绑定", Some(hint));
    }

    facts
}

fn check_daemon(report: &mut Report) -> bool {
    match InstanceGuard::try_acquire("daemon") {
        Ok(Some(_guard)) => {
            report.warn("daemon", "未运行（daemon.lock 空闲）", None);
            false
        }
        Ok(None) => {
            report.ok("daemon", "运行中（daemon.lock 被占用）");
            true
        }
        Err(e) => {
            report.warn("daemon", format!("无法检查单实例锁：{e:#}"), None);
            false
        }
    }
}

fn check_autostart(report: &mut Report, daemon: bool, systemd: &SystemdFacts, niri: &NiriFacts) {
    if systemd.enabled && niri.spawn_valid {
        report.warn(
            "自启",
            "systemd 与 niri spawn-at-startup 同时配置",
            Some("二选一即可；flock 会防双开，但重复启动会多一条失败日志"),
        );
    }
    let autostart = systemd.enabled || niri.spawn_valid;
    match (daemon, autostart) {
        (true, true) => report.ok("自启", "daemon 运行中且已配置自启"),
        (true, false) => report.warn(
            "自启",
            "daemon 在运行但未配置开机自启",
            Some("推荐 systemctl --user enable niri-clip.service"),
        ),
        (false, true) => report.warn(
            "daemon",
            "已配置自启但当前未运行",
            Some(
                "systemctl --user start niri-clip.service；失败时看 journalctl --user -u niri-clip -n 50",
            ),
        ),
        (false, false) => report.fail(
            "daemon",
            "未运行且未配置自启，剪贴板不会被捕获",
            Some("推荐：niri-clip install-service && systemctl --user enable --now niri-clip.service"),
        ),
    }
}

fn fzf_version() -> Option<String> {
    let out = Command::new("fzf").arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn check_backend(report: &mut Report, cfg: &Config) {
    let backend = tui::backend(cfg);
    let fzf = which::which("fzf").ok();
    let fuzzel = which::which("fuzzel").ok();
    let gui = tui::gui_binary();

    match backend {
        "native" => match &gui {
            Some(path) => report.ok("后端", format!("native（{}）", path.display())),
            None if fzf.is_some() || fuzzel.is_some() => report.warn(
                "后端",
                "tui_backend=native 但找不到 niri-clip-gui，将回退终端后端",
                Some("安装 niri-clip-gui，或改 tui_backend=auto"),
            ),
            None => report.fail(
                "后端",
                "tui_backend=native 且找不到 niri-clip-gui / fzf / fuzzel",
                Some("安装 niri-clip-gui，或安装 fzf/fuzzel 后改 tui_backend"),
            ),
        },
        "fzf" => {
            if let Some(path) = &fzf {
                match fzf_version() {
                    Some(version)
                        if tui::parse_version(&version).is_some_and(|v| v >= tui::FZF_MIN) =>
                    {
                        report.ok("后端", format!("fzf {version}（{}）", path.display()));
                    }
                    Some(version) if fuzzel.is_some() => report.warn(
                        "后端",
                        format!("fzf {version} 低于 0.71，运行时会回退 fuzzel"),
                        Some("升级 fzf（--id-nth 需要 ≥0.71）"),
                    ),
                    Some(version) => report.fail(
                        "后端",
                        format!("fzf {version} 低于 0.71 且无 fuzzel 兜底"),
                        Some("升级 fzf >=0.71，或安装 fuzzel"),
                    ),
                    None => report.warn("后端", "fzf 存在但 `fzf --version` 执行失败", None),
                }
            } else if fuzzel.is_some() {
                report.warn(
                    "后端",
                    "fzf 缺失，运行时会回退 fuzzel",
                    Some("安装 fzf >=0.71 以恢复删除/固定后不跳顶"),
                );
            } else {
                report.fail(
                    "后端",
                    "tui_backend=fzf 但找不到 fzf / fuzzel",
                    Some("安装 fzf >=0.71，或安装 fuzzel"),
                );
            }
        }
        "fuzzel" => {
            if fuzzel.is_some() {
                report.ok("后端", "fuzzel");
            } else {
                report.fail(
                    "后端",
                    "tui_backend=fuzzel 但 PATH 中找不到 fuzzel",
                    Some("安装 fuzzel，或改 tui_backend=auto/fzf"),
                );
            }
        }
        other => report.warn("后端", format!("无法识别 {other}，按 auto 处理"), None),
    }

    if backend == "native" && which::which("niri").is_err() {
        report.warn(
            "GUI 聚焦",
            "找不到 niri CLI；Mod+V 连按无法聚焦已开窗口",
            Some("确保 niri 在 PATH 中"),
        );
    }
    if gui.is_none() && backend != "native" {
        report.info("原生 GUI", "未安装 niri-clip-gui（auto 将使用终端后端）");
    }
}

fn check_runtime_deps(report: &mut Report, cfg: &Config) {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        report.ok("Wayland", "WAYLAND_DISPLAY 已设置");
    } else {
        report.warn(
            "Wayland",
            "未检测到 WAYLAND_DISPLAY；静态检查可用，实际捕获需在 niri 会话内验证",
            None,
        );
    }

    if which::which("wl-paste").is_ok() {
        report.ok("wl-paste", "已安装（事件驱动主路径）");
    } else {
        report.warn(
            "wl-paste",
            "未安装，daemon 将退化为 500ms 轮询兜底",
            Some("安装 wl-clipboard"),
        );
    }
    if which::which("wl-copy").is_ok() {
        report.ok("wl-copy", "已安装");
    } else {
        report.fail(
            "wl-copy",
            "未安装，选中条目无法写回剪贴板",
            Some("安装 wl-clipboard"),
        );
    }
    if which::which("timeout").is_ok() {
        report.ok("timeout", "已安装（捕获子进程超时边界）");
    } else {
        report.fail(
            "timeout",
            "未安装，daemon 的捕获子进程无法按时划界",
            Some("安装 coreutils"),
        );
    }

    if cfg.enable_image_preview {
        if which::which("chafa").is_ok() {
            report.ok("chafa", "已安装（终端图片预览）");
        } else {
            report.warn(
                "chafa",
                "未安装，终端图片预览不可用（GUI 不受影响）",
                Some("安装 chafa，或关闭 enable_image_preview"),
            );
        }
    } else {
        report.info("图片捕获", "enable_image_preview=false（默认关闭）");
    }

    if cfg.notify_enabled {
        if which::which("notify-send").is_ok() {
            report.ok("notify-send", "已安装");
        } else {
            report.warn(
                "notify-send",
                "未安装，桌面通知会被静默跳过",
                Some("安装 libnotify，或关闭 notify_enabled"),
            );
        }
    } else {
        report.info("通知", "notify_enabled=false");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_separate_spawn_and_one_line_bind() {
        let text = r#"
            // spawn-at-startup "niri-clip daemon"
            spawn-at-startup "niri-clip" "daemon"
            binds {
                Mod+V { spawn "niri-clip" "tui"; }
            }
        "#;
        let facts = parse_niri_config(text);
        assert!(facts.spawn_valid);
        assert!(!facts.spawn_invalid_single);
        assert!(facts.bind_found);
        assert_eq!(facts.bind_key.as_deref(), Some("Mod+V"));
    }

    #[test]
    fn parses_multiline_bind() {
        let text = r#"
            binds {
                Mod+V {
                    spawn "niri-clip" "tui"
                }
            }
        "#;
        let facts = parse_niri_config(text);
        assert_eq!(facts.bind_key.as_deref(), Some("Mod+V"));
        assert!(facts.bind_found);
    }

    #[test]
    fn flags_single_string_spawn() {
        let facts = parse_niri_config(r#"spawn-at-startup "niri-clip daemon""#);
        assert!(facts.spawn_invalid_single);
        assert!(!facts.spawn_valid);
    }

    #[test]
    fn flags_spawn_without_daemon() {
        let facts = parse_niri_config(r#"spawn-at-startup "niri-clip""#);
        assert!(facts.spawn_without_daemon);
        assert!(!facts.spawn_valid);
    }

    #[test]
    fn ignores_commented_lines() {
        let text = r#"
            // spawn-at-startup "niri-clip" "daemon"
            // Mod+V { spawn "niri-clip" "tui"; }
        "#;
        let facts = parse_niri_config(text);
        assert!(!facts.spawn_valid);
        assert!(!facts.bind_found);
    }

    #[test]
    fn ignores_bind_spawning_daemon() {
        let facts = parse_niri_config(r#"Mod+V { spawn "niri-clip" "daemon"; }"#);
        assert!(!facts.bind_found);
    }

    #[test]
    fn detects_bind_without_explicit_tui() {
        let facts = parse_niri_config(r#"Mod+V { spawn "niri-clip"; }"#);
        assert!(facts.bind_found);
        assert_eq!(facts.bind_key.as_deref(), Some("Mod+V"));
    }

    #[test]
    fn detects_include_and_other_key() {
        let text = r#"
            include "binds.kdl"
            binds {
                Super+Shift+V { spawn "niri-clip" "tui"; }
            }
        "#;
        let facts = parse_niri_config(text);
        assert_eq!(facts.includes, vec!["binds.kdl"]);
        assert_eq!(facts.bind_key.as_deref(), Some("Super+Shift+V"));
    }

    #[test]
    fn resolves_relative_include_and_skips_globs() {
        let config = Path::new("/home/u/.config/niri/config.kdl");
        assert_eq!(
            resolve_include(config, "binds.kdl"),
            Some(PathBuf::from("/home/u/.config/niri/binds.kdl"))
        );
        assert_eq!(resolve_include(config, "*.kdl"), None);
    }

    #[test]
    fn parses_systemd_version() {
        assert_eq!(
            parse_systemd_version("systemd 257 (257.8-1-arch)"),
            Some(257)
        );
        assert_eq!(parse_systemd_version("systemd 248"), Some(248));
        assert_eq!(parse_systemd_version("garbage"), None);
    }
}
