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

use iced::widget::{column, container, scrollable, text, text_input};
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
    /// 搜索框内容变化
    Query(String),
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
    search_id: iced::widget::Id,
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
            search_id: iced::widget::Id::unique(),
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
            Message::Query(q) => {
                self.query = q;
                self.selected = 0;
                self.confirm_delete = false;
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
        match key {
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => self.move_selection(-1),
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => self.move_selection(1),
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
                // 有输入时数字回落为查询字符（text_input 自行处理）
                let n: usize = c.parse().unwrap_or(0);
                if n >= 1 && n <= self.filtered().len() {
                    self.selected = n - 1;
                    return self.update(Message::Copy { exit: true });
                }
            }
            _ => {}
        }
        Task::none()
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

        let search = text_input("搜索（英文直打 / Ctrl-V 粘贴，子序列匹配）…", &self.query)
            .id(self.search_id.clone())
            .on_input(Message::Query)
            .on_paste(Message::Query)
            .on_submit(Message::Copy { exit: true })
            .size(14)
            .padding(6);

        let rows = filtered.iter().enumerate().map(|(idx, clip)| {
            let selected = idx == self.selected;
            let cursor = if selected { "❯" } else { " " };
            let cur_mark = if cur.as_deref() == Some(clip.hash.as_str()) {
                "▶"
            } else {
                " "
            };
            let star = if clip.pinned { "★" } else { " " };
            // 空查询时展示 1-9 快选序号；有输入时列位让渡给过滤结果
            let quick = if self.query.is_empty() && idx < 9 {
                format!("{}", idx + 1)
            } else {
                " ".to_string()
            };
            let line = format!(
                "{cursor} {quick} {cur_mark}{star} {}",
                preview::preview_text(clip, self.preview_width)
            );
            container(text(line).size(14))
                .width(Length::Fill)
                .padding([2, 8])
                .style(move |theme| row_style(selected, theme))
                .into()
        });

        let mut col = column![].push(search).push(
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

        // 底部预览窗格：选中条目全文（多行截断）
        col = col.push(
            container(text(self.preview_selected(&filtered)).size(12))
                .width(Length::Fill)
                .padding([6, 8])
                .style(preview_style),
        );

        col.width(Length::Fill).into()
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

fn row_style(selected: bool, theme: &iced::Theme) -> container::Style {
    let palette = theme.palette();
    container::Style {
        background: Some(Background::Color(if selected {
            palette.primary
        } else {
            Color::TRANSPARENT
        })),
        text_color: Some(if selected {
            palette.background
        } else {
            palette.text
        }),
        ..Default::default()
    }
}

fn preview_style(theme: &iced::Theme) -> container::Style {
    let palette = theme.palette();
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.35,
            ..palette.background
        })),
        text_color: Some(palette.text),
        ..Default::default()
    }
}

fn confirm_style(theme: &iced::Theme) -> container::Style {
    let palette = theme.palette();
    container::Style {
        background: Some(Background::Color(Color {
            a: 0.6,
            ..palette.danger
        })),
        text_color: Some(palette.background),
        ..Default::default()
    }
}

fn main() -> Result<(), iced_exwlshell::Error> {
    let connection = Connection::connect_to_env().expect("no wayland connection");
    let with_connection = connection.clone();
    daemon(
        || {
            let app = App::new();
            // 搜索框自动聚焦：键入即过滤
            let focus = iced::widget::operation::focus(app.search_id.clone());
            (app, focus)
        },
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
