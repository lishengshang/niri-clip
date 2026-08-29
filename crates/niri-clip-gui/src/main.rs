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

use iced::widget::{
    column, container, image, mouse_area, operation, row, rule, scrollable, space, text,
    text_input,
};
use iced::{
    border, keyboard, Background, Border, Color, Element, Font, Length, Shadow, Subscription,
    Task, Vector,
};
use niri_clip_core::{config, preview, store};

/// 主字体：JetBrainsMono Nerd Font（真机已装）。
/// 不用 Font::MONOSPACE（fontconfig 解析到 Noto Sans Mono）：❯▶◆⏎ 等符号
/// 字形缺失且 cosmic-text fallback 不稳 → 显示方框（tofu）。
const UI_FONT: Font = Font::with_name("JetBrainsMono Nerd Font");

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
    /// 鼠标悬停行：跟随选中（高亮预览）
    Hover(usize),
    /// 鼠标真实移动：恢复悬停跟随
    MouseMove,
    /// 鼠标点击行：定位并复制关闭（对齐 Enter 语义）
    Pick(usize),
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
    /// 行列表 scrollable 的 Id：键盘导航时 scroll_to 跟随选中
    list_id: iced::widget::Id,
    clips: Vec<store::Clip>,
    query: String,
    selected: usize,
    /// 星标条目删除二段确认（对齐路线图 1.5：内嵌确认，去 fuzzel 依赖）
    confirm_delete: bool,
    preview_width: usize,
    /// 鼠标悬停跟随开关：键盘导航时关闭——列表在静止指针下滑动会
    /// 逐行触发 on_enter，悬停跟随会反复抢走选中态（界面闪烁根因）
    mouse_follow: bool,
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
            list_id: iced::widget::Id::unique(),
            clips,
            query: String::new(),
            selected: 0,
            confirm_delete: false,
            preview_width: cfg.preview_width,
            mouse_follow: false,
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
                // 重新过滤后回到顶部
                return self.scroll_to_selected();
            }
            Message::Key(key, modifiers) => return self.on_key(key, modifiers),
            Message::Hover(idx) => {
                // 仅在鼠标活跃时跟随：键盘滚动中忽略 on_enter（防闪烁）
                if self.mouse_follow && idx < self.filtered().len() {
                    self.selected = idx;
                }
            }
            Message::MouseMove => {
                self.mouse_follow = true;
            }
            Message::Pick(idx) => {
                // 点击行 = 定位到该行并复制关闭（对齐 Enter）
                if idx < self.filtered().len() {
                    self.selected = idx;
                    return Task::batch([
                        self.scroll_to_selected(),
                        self.update(Message::Copy { exit: true }),
                    ]);
                }
            }
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
                return Task::batch([
                    self.scroll_to_selected(),
                    run_bg(
                        move || store::list(Self::load_limit()).ok(),
                        Message::ListReloaded,
                    ),
                ]);
            }
            Message::ListReloaded(Some(clips)) => {
                self.clips = clips;
                if self.selected >= self.clips.len() {
                    self.selected = self.clips.len().saturating_sub(1);
                }
                return self.scroll_to_selected();
            }
            Message::ListReloaded(None) => {}
        }
        Task::none()
    }

    fn on_key(&mut self, key: keyboard::Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        // 仅导航/动作键；普通字符、Backspace、Space、IME 提交由持焦点的
        // text_input 自行处理（winit 原生 IME，中文可用）
        match key {
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => {
                return self.move_selection(-1)
            }
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => {
                return self.move_selection(1)
            }
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

    fn move_selection(&mut self, delta: i32) -> Task<Message> {
        // 键盘导航接管选中：暂停悬停跟随（列表滚过静止指针会触发
        // 一串 on_enter，把选中态抢回去——闪烁根因）
        self.mouse_follow = false;
        let n = self.filtered().len();
        if n > 0 {
            let next = self.selected as i64 + delta as i64;
            self.selected = next.clamp(0, n as i64 - 1) as usize;
            // 键盘导航滚动跟随：把选中行滚进可视区（视口半高估算）
            return self.scroll_to_selected();
        }
        Task::none()
    }

    /// 把选中行滚动到可视区中部（行高定长 ROW_PITCH，偏移可精确计算）
    fn scroll_to_selected(&self) -> Task<Message> {
        let y = ((self.selected as f32) * ROW_PITCH - VIEWPORT_HALF).max(0.0);
        operation::scroll_to(
            self.list_id.clone(),
            operation::AbsoluteOffset { x: 0.0, y },
        )
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
        let q_lower = self.query.to_lowercase();

        // 视觉对齐 fzf TUI：info 头行 → "剪贴板> " 提示符 → 行列表 → 底部预览
        let header = "1-9快选 · Enter复制 · Ctrl-Y连复 · Ctrl-P固定 · Ctrl-X删除 · Esc清除/退出";
        let counter = format!("{}/{}", filtered.len(), self.clips.len());

        let rows = filtered.iter().enumerate().map(|(idx, clip)| {
            let selected = idx == self.selected;
            let cursor = if selected { "❯" } else { " " };
            let cur_mark = if cur.as_deref() == Some(clip.hash.as_str()) {
                "▶"
            } else {
                " "
            };
            let star = if clip.pinned { "◆" } else { " " };
            let quick = if self.query.is_empty() && idx < 9 {
                format!("{}", idx + 1)
            } else {
                " ".to_string()
            };
            let prefix = format!("{cursor} {quick} {cur_mark}{star} ");
            // ↵（U+21B5）字形覆盖差（tofu），GUI 侧换成 ⏎
            let preview =
                preview::preview_text(clip, self.preview_width).replace('↵', "⏎");
            let base = if selected { ROW_FG_SELECTED } else { ROW_FG };

            // fzf 灵魂：命中查询子序列的字符用 hl 色点亮
            let flags = if self.query.is_empty() {
                None
            } else {
                fuzzy_flags(&q_lower, &preview)
            };

            let mut spans: Vec<text::Span<'static, (), Font>> = Vec::new();
            spans.push(text::Span::new(prefix).color(base));
            let mut run = String::new();
            let mut run_hit = false;
            for (i, ch) in preview.chars().enumerate() {
                let hit = flags.as_ref().is_some_and(|f| f.get(i).copied().unwrap_or(false));
                if i > 0 && hit != run_hit {
                    spans.push(
                        text::Span::new(std::mem::take(&mut run))
                            .color(if run_hit { HL } else { base }),
                    );
                }
                run_hit = hit;
                run.push(ch);
            }
            if !run.is_empty() {
                spans.push(text::Span::new(run).color(if run_hit { HL } else { base }));
            }

            // 鼠标交互：悬停跟随选中，点击复制关闭
            let row = mouse_area(
                container(
                    text::Rich::with_spans(spans)
                        .size(14)
                        .font(UI_FONT)
                        .width(Length::Fill)
                        .wrapping(text::Wrapping::None),
                )
                .width(Length::Fill)
                .height(Length::Fixed(ROW_HEIGHT))
                .padding([4, 10])
                .style(move |_| container::Style {
                    background: Some(Background::Color(if selected { SEL_BG } else { BG })),
                    border: Border {
                        radius: RADIUS_ROW,
                        ..Default::default()
                    },
                    shadow: if selected { SHADOW_ROW } else { Shadow::default() },
                    ..Default::default()
                }),
            )
            .on_press(Message::Pick(idx))
            .on_enter(Message::Hover(idx))
            .on_move(|_| Message::MouseMove);

            Element::from(row)
        });

        // 行列表：行间细分界线（fzf 行分隔观感）
        let mut list = column![];
        for (idx, row) in rows.enumerate() {
            if idx > 0 {
                list = list.push(rule::horizontal(1.0).style(rule_style));
            }
            list = list.push(row);
        }

        let mut col = column![]
            .spacing(6)
            .push(
                container(
                    row![
                        text(header).size(11).color(MUTED),
                        space::horizontal(),
                        text(counter).size(11).color(MUTED)
                    ]
                    .width(Length::Fill),
                )
                .width(Length::Fill)
                .padding([6, 10]),
            )
            .push(
                // fzf 式提示符行：无输入框边框，前缀 + 透明输入区
                row![
                    text("剪贴板> ").size(14).color(ACCENT),
                    text_input("", &self.query)
                        .id(self.search_id.clone())
                        .on_input(Message::Query)
                        .on_paste(Message::Query)
                        .size(14)
                        .font(UI_FONT)
                        .padding([7, 0])
                        .style(prompt_style)
                ]
                .width(Length::Fill)
                .padding([0, 10]),
            )
            .push(rule::horizontal(1.0).style(rule_style))
            .push(
                scrollable(list)
                    .id(self.list_id.clone())
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .style(scroll_style),
            );

        if self.confirm_delete {
            col = col.push(
                container(text("◆ 星标条目删除确认：再按 Ctrl-X 执行，Esc 取消").size(12))
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

/// 同 fuzzy_match，但返回 t 每个字符是否命中（Vec 与 t.chars() 对齐）。
/// 贪心逐字符推进；查询未耗尽 → None（整行不高亮）。
fn fuzzy_flags(q: &str, t: &str) -> Option<Vec<bool>> {
    let mut qc = q.chars();
    let mut next = qc.next();
    let mut flags = Vec::with_capacity(t.chars().count());
    for ch in t.chars() {
        let hit = next.is_some_and(|n| n == ch);
        if hit {
            next = qc.next();
        }
        flags.push(hit);
    }
    next.is_none().then_some(flags)
}

/// 常见位图魔数：PNG / JPEG / GIF / WebP / BMP。
/// 魔数不对直接拒绝——iced image 解码失败会在渲染线程 panic，不能赌。
fn is_image_magic(b: &[u8]) -> bool {
    b.starts_with(&[0x89, b'P', b'N', b'G'])
        || b.starts_with(&[0xFF, 0xD8, 0xFF])
        || b.starts_with(b"GIF8")
        || (b.len() > 12 && b.starts_with(b"RIFF") && b[8..12] == *b"WEBP")
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
/// 命中字符高亮（fzf 默认 hl：深红）
const HL: Color = Color {
    r: 0.949,
    g: 0.467,
    b: 0.478,
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

/// 行定高：Rich 文本 14px × 行高 1.3 ≈ 18.2 + 上下 padding 8，凑整 27。
/// 分界线 1px → 行距（pitch）恒定 28，键盘导航的 scroll_to 据此精确计算
const ROW_HEIGHT: f32 = 27.0;
const ROW_PITCH: f32 = ROW_HEIGHT + 1.0;
/// 视口半高估算（675 - 头行/提示符/预览窗格），选中行滚到可视区中部
const VIEWPORT_HALF: f32 = 240.0;

/// 面板阴影（立体感）：黑色低透明 + 垂直偏移
const SHADOW_PANEL: Shadow = Shadow {
    color: Color {
        a: 0.35,
        r: 0.0,
        g: 0.0,
        b: 0.0,
    },
    offset: Vector {
        x: 0.0,
        y: 2.0,
    },
    blur_radius: 10.0,
};
/// 选中行微阴影
const SHADOW_ROW: Shadow = Shadow {
    color: Color {
        a: 0.30,
        r: 0.0,
        g: 0.0,
        b: 0.0,
    },
    offset: Vector { x: 0.0, y: 1.0 },
    blur_radius: 5.0,
};

fn prompt_style(_theme: &iced::Theme, _status: text_input::Status) -> text_input::Style {
    text_input::Style {
        // 无边框无底色，融入提示符行（fzf 的输入就是一段裸文本）
        background: Background::Color(BG),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::default(),
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

fn rule_style(_theme: &iced::Theme) -> rule::Style {
    rule::Style {
        color: BORDER,
        radius: border::Radius::default(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

fn scroll_style(_theme: &iced::Theme, status: scrollable::Status) -> scrollable::Style {
    // 滚动条默认隐藏，仅当鼠标悬停/拖动滚动条本身时浮现
    let bar_visible = match status {
        scrollable::Status::Active { .. } => false,
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered,
            ..
        } => is_vertical_scrollbar_hovered,
        scrollable::Status::Dragged {
            is_vertical_scrollbar_dragged,
            ..
        } => is_vertical_scrollbar_dragged,
    };
    let hidden_rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
        },
    };
    let v_rail = if bar_visible {
        scrollable::Rail {
            background: None,
            border: Border::default(),
            scroller: scrollable::Scroller {
                background: Background::Color(SCROLLBAR),
                border: Border {
                    radius: border::Radius::from(4.0),
                    ..Default::default()
                },
            },
        }
    } else {
        hidden_rail
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: v_rail,
        horizontal_rail: hidden_rail,
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
        // 浮起的面板：立体感
        shadow: SHADOW_PANEL,
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

fn theme_dark(_state: &App) -> iced::Theme {
    iced::Theme::Dark
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
    // 深色主题（光标/默认控件色）+ JetBrainsMono NF：符号字形齐全无 tofu
    .theme(theme_dark)
    .default_font(UI_FONT)
    .subscription(App::subscription)
    .window(iced::window::Settings {
        size: iced::Size::new(500.0, 675.0),
        resizable: true,
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: String::from("niri-clip-gui"),
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}
