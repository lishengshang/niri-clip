//! niri-clip 原生 layer-shell 前端（ADR-001：iced_exwlshell，按需进程形态）。
//!
//! 语义与 fzf TUI 对齐（M5.3）：
//! - 搜索过滤：fzf 风格子序列匹配（大小写归一，中文按字符）；有输入时数字
//!   回落为查询字符，空查询时 1-9 快选复制
//! - 导航：↑↓ 移动 ❯，Enter 复制并关闭，Ctrl-Y 连续复制（▶ 跟随）
//! - 管理：Ctrl-P 固定/取消，Ctrl-X 删除（星标条目需二段确认，对齐路线图 1.5）
//! - 底部预览窗格展示选中条目全文（截断）
//!
//! 已知限制（ADR-001 附录）：
//! - IME：iced_exwlshell 的 daemon 路径未接 zwp_text_input（上游缺口，
//!   multi_window 运行器私有不可达）。中文搜索场景由默认 fzf TUI 后端覆盖；
//!   GUI 搜索走英文直打或 Ctrl-V 粘贴（剪贴板管理器的自然交互）
//! - 阻塞：复制/固定/删除全部后台线程化（Task::perform + worker），
//!   UI 线程零阻塞——同步 wait 是偶发卡死根因
//!
//! 分层约定：业务全部在 core（store::copy_to_clipboard / delete / toggle_pin /
//! current 指针），本 crate 只做渲染与输入分发。

use iced::widget::{column, container, scrollable, text};
use iced::{keyboard, Background, Color, Element, Length, Subscription, Task};
use iced_exwlshell::reexport::{Anchor, KeyboardInteractivity};
use iced_exwlshell::settings::{LayerShellSettings, Settings};
use iced_exwlshell::{daemon, to_exwlshell_message};
use niri_clip_core::{config, preview, store};
use wayland_client::Connection;

// 注意顺序：to_exwlshell_message 必须在 derive 之前（外层属性宏先增广枚举，
// derive(Debug/Clone) 才能覆盖宏注入的 shell 变体）
#[to_exwlshell_message]
#[derive(Debug, Clone)]
enum Message {
    /// Ctrl-V：读取系统剪贴板追加进查询
    Paste(String),
    /// 全局键盘路由：具体语义在 update 里结合状态分发
    Key(keyboard::Key, keyboard::Modifiers),
    /// 复制当前选中条目；Enter 复制后关闭窗口，Ctrl-Y 连续复制不退出
    Copy { exit: bool },
    /// 后台复制完成
    CopyFinished { exit: bool, ok: bool },
    /// 后台 pin/delete 完成，带回重拉后的列表（None = worker 异常，放弃）
    ListReloaded(Option<Vec<store::Clip>>),
    /// Esc 关闭窗口
    Exit,
}

/// 把阻塞任务丢到后台线程执行（iced 默认执行器为 thread-pool，
/// Task::perform 的 future 里阻塞仅占一个 worker，UI 线程不受影响）。
/// worker panic 时以 None 回传，UI 放弃本次结果不卡死。
fn run_bg<T, F>(f: F, wrap: impl Fn(T) -> Message + Send + 'static) -> Task<Message>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Task::perform(
        async move {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(f());
            });
            rx.recv().ok()
        },
        move |t| match t {
            Some(v) => wrap(v),
            None => Message::ListReloaded(None),
        },
    )
}

struct App {
    clips: Vec<store::Clip>,
    query: String,
    selected: usize,
    /// 星标条目删除二段确认（对齐路线图 1.5：内嵌确认，去 fuzzel 依赖）
    confirm_delete: bool,
    preview_width: usize,
}

impl App {
    fn new() -> Self {
        let cfg = config::Config::load();
        let clips = store::list(Self::load_limit()).unwrap_or_default();
        Self {
            clips,
            query: String::new(),
            selected: 0,
            confirm_delete: false,
            preview_width: cfg.preview_width,
        }
    }

    fn load_limit() -> usize {
        let cfg = config::Config::load();
        cfg.max_items.min(store::TUI_LIMIT)
    }

    /// 过滤后的视图：fzf 风格子序列匹配（大小写归一，中文按字符）
    fn filtered(&self) -> Vec<&store::Clip> {
        if self.query.is_empty() {
            return self.clips.iter().collect();
        }
        let q = self.query.to_lowercase();
        self.clips
            .iter()
            .filter(|c| fuzzy_match(&q, &c.text.to_lowercase()))
            .collect()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Paste(s) => {
                self.query.push_str(&s);
                self.selected = 0;
            }
            Message::Key(key, modifiers) => return self.on_key(key, modifiers),
            Message::Copy { exit } => {
                // 复制链路（wl-copy fork/wait + sqlite 读）全部后台化，
                // UI 线程零阻塞——偶发卡死的根因即此处的同步 wait
                let target = self.filtered().get(self.selected).map(|c| c.id);
                if let Some(id) = target {
                    return run_bg(
                        move || store::copy_to_clipboard(id).is_ok(),
                        move |ok| Message::CopyFinished { exit, ok },
                    );
                }
            }
            Message::CopyFinished { exit, ok } => {
                if !ok {
                    eprintln!("[niri-clip gui] copy failed");
                }
                if exit {
                    // 后台复制已完成，wl-copy 守护进程持有数据，退出安全
                    std::process::exit(0);
                }
                // Ctrl-Y 连续复制：重拉列表，▶ 已随 copy 刷新到刚复制的条目
                self.query.clear();
                self.selected = 0;
                return run_bg(
                    move || store::list(Self::load_limit()).ok(),
                    Message::ListReloaded,
                );
            }
            Message::ListReloaded(Some(clips)) => {
                self.clips = clips;
                if self.selected >= self.clips.len() {
                    self.selected = self.clips.len().saturating_sub(1);
                }
            }
            Message::ListReloaded(None) => {}
            Message::Exit => std::process::exit(0),
            _ => {}
        }
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        self.log_key(&key, &modifiers);
        // 文本输入由全局键盘处理器直接接管（fzf 模式），不依赖 text_input
        // widget 焦点——operation::focus 在 exwlshell 下不可靠（真机失效），
        // widget 只负责展示 self.query
        match key {
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => self.move_selection(-1),
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => self.move_selection(1),
            keyboard::Key::Named(keyboard::key::Named::Backspace) => {
                self.query.pop();
            }
            keyboard::Key::Named(keyboard::key::Named::Space) => self.query.push(' '),
            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                // fzf 语义：有输入/确认时先取消，空查询才退出
                if !self.query.is_empty() || self.confirm_delete {
                    self.query.clear();
                    self.confirm_delete = false;
                } else {
                    std::process::exit(0);
                }
            }
            keyboard::Key::Named(keyboard::key::Named::Enter) => {
                return self.update(Message::Copy { exit: true });
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "y" => {
                return self.update(Message::Copy { exit: false });
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "p" => {
                // 后台 pin + 重拉列表，UI 线程零阻塞
                let Some(clip) = self.filtered().get(self.selected).cloned() else {
                    return Task::none();
                };
                let id = clip.id;
                return run_bg(
                    move || {
                        let ok = store::toggle_pin(id).is_ok();
                        (ok, store::list(App::load_limit()).ok())
                    },
                    |(ok, clips)| {
                        if !ok {
                            eprintln!("[niri-clip gui] pin failed");
                        }
                        Message::ListReloaded(clips)
                    },
                );
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "v" => {
                // 粘贴搜索（剪贴板管理器的自然交互）：读系统剪贴板追加进查询
                return iced::clipboard::read().map(|s| Message::Paste(s.unwrap_or_default()));
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "x" => {
                return self.delete_selected();
            }
            keyboard::Key::Character(c)
                if self.query.is_empty()
                    && !modifiers.control()
                    && c.len() == 1
                    && c.as_str() >= "1"
                    && c.as_str() <= "9" =>
            {
                // 空查询时 1-9 快选：定位到过滤列表第 n 行并复制关闭；
                // 有输入时数字回落为查询字符（下方通用分支处理）
                let n: usize = c.parse().unwrap_or(0);
                if n >= 1 && n <= self.filtered().len() {
                    self.selected = n - 1;
                    return self.update(Message::Copy { exit: true });
                }
            }
            keyboard::Key::Character(c)
                if !modifiers.control() && !modifiers.alt() && !modifiers.logo() =>
            {
                // 普通可打印字符直接进查询（widget 不持有焦点也能输入）
                for ch in c.chars() {
                    if !ch.is_control() {
                        self.query.push(ch);
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    /// 临时诊断日志：真机按键问题定位用（~/.local/state/niri-clip/gui.log）
    fn log_key(&self, key: &keyboard::Key, modifiers: &keyboard::Modifiers) {
        use std::io::Write;
        let path = config::Config::state_dir().join("gui.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "key={key:?} mods={modifiers:?}");
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let n = self.filtered().len();
        if n > 0 {
            let next = self.selected as i64 + delta as i64;
            self.selected = next.clamp(0, n as i64 - 1) as usize;
        }
    }

    fn delete_selected(&mut self) -> Task<Message> {
        let Some(clip) = self.filtered().get(self.selected).cloned() else {
            return Task::none();
        };
        // 星标条目二段确认：第一次 Ctrl-X 仅挂起确认，再按才执行
        if clip.pinned && !self.confirm_delete {
            self.confirm_delete = true;
            return Task::none();
        }
        let id = clip.id;
        // 后台删除 + 重拉列表，UI 线程零阻塞（sqlite 写锁最长 busy_timeout 5s）
        self.confirm_delete = false;
        self.query.clear();
        run_bg(
            move || {
                let ok = store::delete(id).is_ok();
                (ok, store::list(App::load_limit()).ok())
            },
            |(ok, clips)| {
                if !ok {
                    eprintln!("[niri-clip gui] delete failed");
                }
                Message::ListReloaded(clips)
            },
        )
    }

    fn view(&self, _id: iced::window::Id) -> Element<'_, Message> {
        let cur = store::current_hash();
        let filtered = self.filtered();

        // 视觉对齐 fzf TUI：header 提示行 → "剪贴板> " 提示符 → 行列表 → 底部预览
        let header = "1-9快选 · Enter复制 · Ctrl-Y连复 · Ctrl-P固定 · Ctrl-X删除 · Esc清除/退出";
        let prompt = format!("剪贴板> {}▏", self.query);

        let rows = filtered.iter().enumerate().map(|(idx, clip)| {
            let selected = idx == self.selected;
            let cursor = if selected { "❯" } else { " " };
            let cur_mark = if cur.as_deref() == Some(clip.hash.as_str()) {
                "▶"
            } else {
                " "
            };
            let star = if clip.pinned { "★" } else { " " };
            let quick = if self.query.is_empty() && idx < 9 {
                format!("{}", idx + 1)
            } else {
                " ".to_string()
            };
            let line = format!(
                "{cursor} {quick} {cur_mark}{star} {}",
                preview::preview_text(clip, self.preview_width)
            );
            container(
                text(line)
                    .size(14)
                    .color(if selected { ROW_FG_SELECTED } else { ROW_FG }),
            )
            .width(Length::Fill)
            .padding([2, 8])
            .style(move |_| container::Style {
                background: Some(Background::Color(if selected { SEL_BG } else { BG })),
                ..Default::default()
            })
            .into()
        });

        let mut col = column![]
            .push(
                container(text(header).size(11).color(ACCENT))
                    .width(Length::Fill)
                    .padding([4, 8]),
            )
            .push(
                container(text(prompt).size(14).color(ACCENT))
                    .width(Length::Fill)
                    .padding([6, 8]),
            )
            .push(
                scrollable(column(rows).width(Length::Fill))
                    .height(Length::Fill)
                    .width(Length::Fill),
            );

        if self.confirm_delete {
            col = col.push(
                container(text("★ 星标条目删除确认：再按 Ctrl-X 执行，Esc 取消").size(12))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(confirm_style),
            );
        }

        // 底部预览窗格：选中条目全文（多行截断），与 TUI 的 preview-window 对应
        col = col.push(
            container(text(self.preview_selected(&filtered)).size(12))
                .width(Length::Fill)
                .padding([6, 8])
                .style(preview_style),
        );

        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(BG)),
                ..Default::default()
            })
            .into()
    }

    /// 底部预览：选中条目全文多行截断（图片条目给数据文件提示）
    fn preview_selected(&self, filtered: &[&store::Clip]) -> String {
        let Some(clip) = filtered.get(self.selected) else {
            return String::from("(空)");
        };
        if clip.mime.starts_with("image/") {
            let path = clip.image_path.as_deref().unwrap_or("(未记录数据文件)");
            return format!("[image {}] {}", clip.mime, path);
        }
        let mut out = String::new();
        for line in clip.text.lines().take(8) {
            let l: String = line.chars().take(160).collect();
            out.push_str(&l);
            out.push('\n');
        }
        if clip.text.len() > out.len() {
            out.push('…');
        }
        out
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                Some(Message::Key(key, modifiers))
            }
            _ => None,
        })
    }
}

/// fzf 风格子序列匹配：q 的字符按序全部出现在 t 中即可（大小写已归一）
fn fuzzy_match(q: &str, t: &str) -> bool {
    let mut chars = t.chars();
    q.chars().all(|qc| chars.any(|tc| tc == qc))
}

// 配色对齐 fzf 默认深色风格
const BG: Color = Color {
    r: 0.086,
    g: 0.086,
    b: 0.110,
    a: 1.0,
};
const ROW_FG: Color = Color {
    r: 0.88,
    g: 0.88,
    b: 0.91,
    a: 1.0,
};
const ROW_FG_SELECTED: Color = Color {
    r: 0.96,
    g: 0.96,
    b: 0.98,
    a: 1.0,
};
const ACCENT: Color = Color {
    r: 0.48,
    g: 0.64,
    b: 0.97,
    a: 1.0,
};
const SEL_BG: Color = Color {
    r: 0.16,
    g: 0.28,
    b: 0.50,
    a: 1.0,
};

fn preview_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            a: 1.0,
            r: BG.r + 0.03,
            g: BG.g + 0.03,
            b: BG.b + 0.04,
        })),
        text_color: Some(ROW_FG),
        ..Default::default()
    }
}

fn confirm_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color {
            r: 0.42,
            g: 0.11,
            b: 0.11,
            a: 1.0,
        })),
        text_color: Some(ROW_FG_SELECTED),
        ..Default::default()
    }
}

fn main() -> Result<(), iced_exwlshell::Error> {
    let connection = Connection::connect_to_env().expect("no wayland connection");
    let with_connection = connection.clone();
    daemon(
        || (App::new(), Task::none()),
        || String::from("niri-clip"),
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .settings(Settings {
        layer_settings: LayerShellSettings {
            // 顶部居中浮层：仅锚定 Top，水平方向不锚定时合成器自动居中；
            // 通栏（Top|Left|Right）会让窗口独占上半屏，已废弃
            anchor: Anchor::Top,
            size: Some((760, 420)),
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..Default::default()
        },
        with_connection: Some(with_connection.into()),
        ..Default::default()
    })
    .run()
}
