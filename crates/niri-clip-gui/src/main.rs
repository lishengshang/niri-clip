//! niri-clip 原生前端（iced xdg 窗口，app-id = "niri-clip-gui"）。
//!
//! 架构修订（ADR-001 修订 1）：由 layer-shell 覆盖层改为常规 xdg 窗口——
//! layer-shell 无法被 niri window-rule 约束，且 daemon 式事件循环缺 IME；
//! xdg 窗口带来：
//! - niri window-rule 全量生效（悬浮/位置/边框/阴影/透明度由用户 rule.kdl 约定）
//! - winit 原生 IME（zwp_text_input）→ 中文搜索可用
//! - 图片预览（iced image widget 直接渲染 images/{id}.bin）
//!
//! 渲染：tiny-skia 纯软件（NVIDIA wgpu 冻结 / GL 启动失败的规避，见 #360）。
//! 分层约定：业务全部在 core（store::copy_to_clipboard / delete / toggle_pin /
//! current 指针），本 crate 只做渲染与输入分发。

use std::cell::RefCell;

use iced::widget::{column, container, image, scrollable, text, text_input};
use iced::{border, keyboard, Background, Border, Color, Element, Length, Subscription, Task};
use niri_clip_core::{config, preview, store};

#[derive(Debug, Clone)]
enum Message {
    /// 搜索框内容变化（widget 持焦点自管键入，winit 原生 IME）
    Query(String),
    /// 全局键盘路由：仅处理导航/动作键，普通字符交给 widget
    Key(keyboard::Key, keyboard::Modifiers),
    /// 复制当前选中条目；Enter 复制后关闭窗口，Ctrl-Y 连续复制不退出
    Copy { exit: bool },
    /// 后台复制完成
    CopyFinished { exit: bool, ok: bool },
    /// 后台 pin/delete 完成，带回重拉后的列表（None = worker 异常，放弃）
    ListReloaded(Option<Vec<store::Clip>>),
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
    /// 图片 Handle 跨帧缓存：(clip id, Handle)。Handle::from_bytes 每次
    /// 调用生成新 Id，若在 view 里现建会导致 tiny-skia 每帧重新解码；
    /// clip id 内容不可变，按 id 缓存安全。view(&self) 下用 RefCell。
    image_cache: RefCell<Option<(i64, image::Handle)>>,
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
            image_cache: RefCell::new(None),
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
                // UI 线程零阻塞——同步 wait 是卡死根因
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
        }
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        // 仅导航/动作键；普通字符、Backspace、Space、IME 提交由持焦点的
        // text_input 自行处理（winit 原生 IME，中文可用）
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

    fn view(&self) -> Element<'_, Message> {
        let cur = store::current_hash();
        let filtered = self.filtered();

        // 视觉对齐 fzf TUI：header 提示行 → 搜索框 → 行列表 → 底部预览
        let header = "1-9快选 · Enter复制 · Ctrl-Y连复 · Ctrl-P固定 · Ctrl-X删除 · Esc清除/退出";

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
            .padding([4, 10])
            .style(move |_| container::Style {
                background: Some(Background::Color(if selected { SEL_BG } else { BG })),
                border: Border {
                    radius: RADIUS_ROW,
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
        });

        let mut col = column![]
            .spacing(6)
            .push(
                container(text(header).size(11).color(MUTED))
                    .width(Length::Fill)
                    .padding([6, 10]),
            )
            .push(
                container(
                    text_input(
                        "剪贴板> 搜索（中文 IME / Ctrl-V 粘贴，子序列匹配）…",
                        &self.query,
                    )
                    .id(self.search_id.clone())
                    .on_input(Message::Query)
                    .on_paste(Message::Query)
                    .size(14)
                    .padding([7, 10])
                    .style(input_style),
                )
                .width(Length::Fill)
                .padding([0, 8]),
            )
            .push(
                scrollable(column(rows).width(Length::Fill).spacing(2))
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .style(scroll_style),
            );

        if self.confirm_delete {
            col = col.push(
                container(text("★ 星标条目删除确认：再按 Ctrl-X 执行，Esc 取消").size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(confirm_style),
            );
        }

        // 底部预览窗格：文本多行截断；图片条目直接渲染（iced image widget）
        let Some(clip) = filtered.get(self.selected) else {
            return container(col)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Background::Color(BG)),
                    ..Default::default()
                })
                .into();
        };
        if clip.mime.starts_with("image/") {
            match self.image_handle(clip) {
                Some(handle) => {
                    col = col.push(
                        container(
                            image(handle).height(Length::Fixed(140.0)),
                        )
                        .width(Length::Fill)
                        .padding([6, 8])
                        .style(preview_style),
                    );
                }
                None => {
                    col = col.push(
                        container(text(format!("[image {}] 数据文件缺失", clip.mime)).size(12))
                            .width(Length::Fill)
                            .padding([6, 8])
                            .style(preview_style),
                    );
                }
            }
        } else {
            col = col.push(
                container(text(self.preview_text(clip)).size(12))
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(preview_style),
            );
        }

        container(col)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_| container::Style {
                background: Some(Background::Color(BG)),
                ..Default::default()
            })
            .into()
    }

    /// 底部预览：选中条目全文多行截断
    fn preview_text(&self, clip: &store::Clip) -> String {
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

    /// 图片条目的渲染 Handle：从 images/{id}.bin 读字节按内容解码
    /// （旧实现 Handle::from_path 按扩展名猜格式，`.bin` 猜不出 →
    /// tiny-skia 渲染线程 panic "Image should be allocated"）。
    /// 无法识别魔数的文件回落 None，显示缺失提示而非崩溃。
    fn image_handle(&self, clip: &store::Clip) -> Option<image::Handle> {
        let mut cache = self.image_cache.borrow_mut();
        if let Some((id, handle)) = cache.as_ref() {
            if *id == clip.id {
                return Some(handle.clone());
            }
        }
        let bytes = clip
            .image_path
            .as_deref()
            .map(std::fs::read)
            .and_then(|r| r.ok())
            .filter(|b| is_image_magic(b))?;
        let handle = image::Handle::from_bytes(bytes);
        *cache = Some((clip.id, handle.clone()));
        Some(handle)
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

/// 常见位图魔数：PNG / JPEG / GIF / WebP / BMP。
/// 魔数不对直接拒绝——iced image 解码失败会在渲染线程 panic，不能赌。
fn is_image_magic(b: &[u8]) -> bool {
    b.starts_with(&[0x89, b'P', b'N', b'G'])
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP")
        || b.starts_with(b"BM")
}

// 配色对齐 fzf 默认深色风格（与 layer-shell 旧版一致的视觉语言）
const BG: Color = Color {
    r: 0.086,
    g: 0.086,
    b: 0.110,
    a: 1.0,
};
/// 面板色：搜索框 / 预览窗格底色（比 BG 微亮一档）
const PANEL: Color = Color {
    r: 0.110,
    g: 0.110,
    b: 0.140,
    a: 1.0,
};
const BORDER: Color = Color {
    r: 0.170,
    g: 0.170,
    b: 0.220,
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
/// 次要文本：header 提示行 / 占位符 / 预览
const MUTED: Color = Color {
    r: 0.545,
    g: 0.570,
    b: 0.650,
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
const SCROLLBAR: Color = Color {
    r: 0.230,
    g: 0.230,
    b: 0.290,
    a: 1.0,
};

const RADIUS_ROW: border::Radius = border::Radius {
    top_left: 4.0,
    top_right: 4.0,
    bottom_right: 4.0,
    bottom_left: 4.0,
};
const RADIUS_PANEL: border::Radius = border::Radius {
    top_left: 6.0,
    top_right: 6.0,
    bottom_right: 6.0,
    bottom_left: 6.0,
};

fn input_style(_theme: &iced::Theme, status: text_input::Status) -> text_input::Style {
    let (bg, border_color) = match status {
        text_input::Status::Focused { .. } => (PANEL, ACCENT),
        text_input::Status::Hovered => (PANEL, BORDER),
        _ => (PANEL, BORDER),
    };
    text_input::Style {
        background: Background::Color(bg),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: RADIUS_PANEL,
        },
        icon: ACCENT,
        placeholder: MUTED,
        value: ROW_FG_SELECTED,
        selection: Color {
            a: 0.35,
            ..ACCENT
        },
    }
}

fn scroll_style(_theme: &iced::Theme, _status: scrollable::Status) -> scrollable::Style {
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(SCROLLBAR),
            border: Border {
                radius: border::Radius::from(4.0),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Background::Color(ACCENT),
            border: Border::default(),
            shadow: Default::default(),
            icon: BG,
        },
    }
}

fn preview_style(_theme: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PANEL)),
        text_color: Some(MUTED),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: RADIUS_PANEL,
        },
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
        border: Border {
            radius: RADIUS_PANEL,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn main() -> iced::Result {
    // 常规 xdg 窗口：受 niri window-rule 约束（悬浮/位置/边框由用户
    // rule.kdl 约定，app-id = "niri-clip-gui"），winit 原生 IME
    iced::application(
        || {
            let app = App::new();
            // 搜索框自动聚焦：键入即过滤（winit 焦点可靠）
            let focus = iced::widget::operation::focus(app.search_id.clone());
            (app, focus)
        },
        App::update,
        App::view,
    )
    .title("niri-clip")
    .subscription(App::subscription)
    .window(iced::window::Settings {
        size: iced::Size::new(760.0, 420.0),
        resizable: true,
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: String::from("niri-clip-gui"),
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}
