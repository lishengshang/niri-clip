//! niri-clip 原生 layer-shell 前端（ADR-001：iced_exwlshell，按需进程形态）。
//!
//! MVP（M5.2）：窗口、列表渲染、键盘导航、Enter 复制并关闭、Ctrl-Y 连续复制、
//! Esc 关闭。语义与 fzf TUI 对齐：▶ 当前项置顶（store::list 已保证）、
//! 星标标记、❯ 选中行。搜索过滤、快选、删除确认在 M5.3。
//!
//! 复制路径复用 core 的 `store::copy_to_clipboard`（wl-copy 与 current 指针刷新），
//! UI 层不自行实现任何业务语义。

use iced::widget::{column, container, scrollable, text};
use iced::{keyboard, Background, Color, Element, Length, Subscription, Task};
use iced_exwlshell::reexport::{Anchor, KeyboardInteractivity};
use iced_exwlshell::settings::{LayerShellSettings, Settings};
use iced_exwlshell::{daemon, to_exwlshell_message};
use niri_clip_core::{config, store};
use wayland_client::Connection;

// 注意顺序：to_exwlshell_message 必须在 derive 之前（外层属性宏先增广枚举，
// derive(Debug/Clone) 才能覆盖宏注入的 shell 变体）
#[to_exwlshell_message]
#[derive(Debug, Clone)]
enum Message {
    /// 光标移动（+1/-1；后续 M5.3 扩展 PageUp/PageDown/快选）
    Move(i32),
    /// 复制当前选中条目；Enter 复制后关闭窗口，Ctrl-Y 连续复制不退出
    Copy { exit: bool },
    /// 关闭窗口（Esc）
    Exit,
}

struct App {
    clips: Vec<store::Clip>,
    selected: usize,
    preview_width: usize,
}

impl App {
    fn new() -> Self {
        let cfg = config::Config::load();
        // store::list 已按"当前项置顶（▶）→ 星标 → 时间倒序"排序，
        // 初始选中 = 第 1 行 = Ctrl+V 会粘出的内容
        let clips = store::list(cfg.max_items.min(store::TUI_LIMIT)).unwrap_or_default();
        Self {
            clips,
            selected: 0,
            preview_width: cfg.preview_width,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Move(delta) => {
                let n = self.clips.len();
                if n > 0 {
                    let next = self.selected as i64 + delta as i64;
                    self.selected = next.clamp(0, n as i64 - 1) as usize;
                }
                Task::none()
            }
            Message::Copy { exit } => {
                if let Some(clip) = self.clips.get(self.selected) {
                    let id = clip.id;
                    if let Err(e) = store::copy_to_clipboard(id) {
                        eprintln!("[niri-clip gui] copy failed: {e:#}");
                    }
                }
                if exit {
                    // wl-copy 已 fork 守护进程持有数据，进程退出不丢内容
                    std::process::exit(0);
                }
                // Ctrl-Y 连续复制：重拉列表，▶ 已随 copy 刷新到刚复制的条目
                let cfg = config::Config::load();
                self.clips = store::list(cfg.max_items.min(store::TUI_LIMIT)).unwrap_or_default();
                self.selected = 0;
                Task::none()
            }
            Message::Exit => std::process::exit(0),
            _ => Task::none(),
        }
    }

    fn view(&self, _id: iced::window::Id) -> Element<'_, Message> {
        let cur = store::current_hash();
        let rows = self.clips.iter().enumerate().map(|(idx, clip)| {
            let selected = idx == self.selected;
            let cursor = if selected { "❯" } else { " " };
            let cur_mark = if cur.as_deref() == Some(clip.hash.as_str()) {
                "▶"
            } else {
                " "
            };
            let star = if clip.pinned { "★" } else { " " };
            let line = format!(
                "{cursor} {cur_mark}{star} {}",
                niri_clip_core::preview::preview_text(clip, self.preview_width)
            );
            container(text(line).size(14))
                .width(Length::Fill)
                .padding([2, 8])
                .style(move |theme| row_style(selected, theme))
                .into()
        });
        scrollable(column(rows).width(Length::Fill)).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some(Message::Move(-1)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some(Message::Move(1)),
                    keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::Exit),
                    keyboard::Key::Named(keyboard::key::Named::Enter) => {
                        Some(Message::Copy { exit: true })
                    }
                    keyboard::Key::Character(c) if modifiers.control() && c == "y" => {
                        Some(Message::Copy { exit: false })
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }
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

fn main() -> Result<(), iced_exwlshell::Error> {
    let connection = Connection::connect_to_env().expect("no wayland connection");
    let with_connection = connection.clone();
    daemon(
        App::new,
        || String::from("niri-clip"),
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .settings(Settings {
        layer_settings: LayerShellSettings {
            // fuzzel 风格：顶部横向铺开、高 420 的浮层，独占键盘
            anchor: Anchor::Top | Anchor::Left | Anchor::Right,
            size: Some((0, 420)),
            keyboard_interactivity: KeyboardInteractivity::Exclusive,
            ..Default::default()
        },
        with_connection: Some(with_connection.into()),
        ..Default::default()
    })
    .run()
}
