//! niri-clip 原生 layer-shell 前端（ADR-001：iced_exwlshell，按需进程形态）。
//!
//! 语义与 fzf TUI 对齐（M5.3）：
//! - 搜索过滤：fzf 风格子序列匹配（大小写归一，中文按字符）；有输入时数字
//!   回落为查询字符，空查询时 1-9 快选复制
//! - 导航：↑↓ 移动 ❯，Enter 复制并关闭，Ctrl-Y 连续复制（▶ 跟随）
//! - 管理：Ctrl-P 固定/取消，Ctrl-X 删除（星标条目需二段确认，对齐路线图 1.5）
//! - 底部预览窗格展示选中条目全文（截断）
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
    /// Esc 关闭窗口
    Exit,
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
        let clips = store::list(cfg.max_items.min(store::TUI_LIMIT)).unwrap_or_default();
        Self {
            search_id: iced::widget::Id::unique(),
            clips,
            query: String::new(),
            selected: 0,
            confirm_delete: false,
            preview_width: cfg.preview_width,
        }
    }

    fn menu_limit(&self) -> usize {
        let cfg = config::Config::load();
        cfg.max_items.min(store::TUI_LIMIT)
    }

    /// 重新拉取列表（保留选中位置，越界时夹到末尾）
    fn reload(&mut self) {
        self.clips = store::list(self.menu_limit()).unwrap_or_default();
        if self.selected >= self.clips.len() {
            self.selected = self.clips.len().saturating_sub(1);
        }
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
            Message::Key(key, modifiers) => self.on_key(key, modifiers),
            Message::Copy { exit } => {
                let target = self.filtered().get(self.selected).map(|c| c.id);
                if let Some(id) = target {
                    if let Err(e) = store::copy_to_clipboard(id) {
                        eprintln!("[niri-clip gui] copy failed: {e:#}");
                    }
                }
                if exit {
                    // wl-copy 已 fork 守护进程持有数据，进程退出不丢内容
                    std::process::exit(0);
                }
                // Ctrl-Y 连续复制：重拉列表，▶ 已随 copy 刷新到刚复制的条目
                self.query.clear();
                self.reload();
                self.selected = 0;
            }
            Message::Exit => std::process::exit(0),
            _ => {}
        }
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) {
        match key {
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => self.move_selection(-1),
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => self.move_selection(1),
            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                // fzf 语义：有输入时 Esc 先清空查询，空查询才退出
                if !self.query.is_empty() || self.confirm_delete {
                    self.query.clear();
                    self.confirm_delete = false;
                } else {
                    std::process::exit(0);
                }
            }
            keyboard::Key::Named(keyboard::key::Named::Enter) => {
                let _ = self.update(Message::Copy { exit: true });
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "y" => {
                let _ = self.update(Message::Copy { exit: false });
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "p" => {
                if let Some(clip) = self.filtered().get(self.selected) {
                    let id = clip.id;
                    if let Err(e) = store::toggle_pin(id) {
                        eprintln!("[niri-clip gui] pin failed: {e:#}");
                    }
                    self.reload();
                }
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "x" => {
                self.delete_selected();
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
                    let _ = self.update(Message::Copy { exit: true });
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let n = self.filtered().len();
        if n > 0 {
            let next = self.selected as i64 + delta as i64;
            self.selected = next.clamp(0, n as i64 - 1) as usize;
        }
    }

    fn delete_selected(&mut self) {
        let Some(clip) = self.filtered().get(self.selected).cloned() else {
            return;
        };
        // 星标条目二段确认：第一次 Ctrl-X 仅挂起确认，再按才执行
        if clip.pinned && !self.confirm_delete {
            self.confirm_delete = true;
            return;
        }
        if let Err(e) = store::delete(clip.id) {
            eprintln!("[niri-clip gui] delete failed: {e:#}");
        }
        self.confirm_delete = false;
        self.query.clear();
        self.reload();
    }

    fn view(&self, _id: iced::window::Id) -> Element<'_, Message> {
        let cur = store::current_hash();
        let filtered = self.filtered();

        let search = text_input("搜索（子序列匹配）…", &self.query)
            .id(self.search_id.clone())
            .on_input(Message::Query)
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
            container(text(self.preview_selected()).size(12))
                .width(Length::Fill)
                .padding([6, 8])
                .style(preview_style),
        );

        col.width(Length::Fill).into()
    }

    /// 底部预览：选中条目全文多行截断（图片条目给数据文件提示）
    fn preview_selected(&self) -> String {
        let filtered = self.filtered();
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
