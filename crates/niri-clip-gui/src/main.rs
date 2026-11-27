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
use std::cmp::Reverse;
use std::time::{Duration, Instant};

mod executor;
mod image_cache;
mod instance;
mod position;
mod search;
mod theme;
mod update;
mod view;
mod warm;

use instance::ensure_single_instance;
use search::{fuzzy_flags, fuzzy_score};
use theme::*;

use iced::widget::{
    container, image, mouse_area, operation, row, rule, scrollable, space, text, text_input,
};
use iced::{event, keyboard, mouse, Background, Border, Element, Font, Length, Subscription, Task};
use niri_clip_core::{config::SelectorConfig, confirm, preview, store};

/// 主字体：JetBrainsMono Nerd Font（真机已装）。
/// 不用 Font::MONOSPACE（fontconfig 解析到 Noto Sans Mono）：❯▶◆⏎ 等符号
/// 字形缺失且 cosmic-text fallback 不稳 → 显示方框（tofu）。
const UI_FONT: Font = Font::with_name("JetBrainsMono Nerd Font");

#[derive(Clone)]
enum Message {
    /// 搜索框内容变化（widget 持焦点自管键入，winit 原生 IME）
    Query(String),
    /// FTS 全库搜索完成（后台线程产出，任务 2.1）：query/gen 双新鲜度
    /// 检查，过期结果直接丢弃（新请求已在途，避免闪烁旧候选）
    SearchDone {
        query: String,
        gen: u64,
        hits: Vec<store::Clip>,
    },
    /// 全局键盘路由：仅处理导航/动作键，普通字符交给 widget
    Key(keyboard::Key, keyboard::Modifiers),
    /// 复制当前选中条目；Enter 复制后关闭窗口，Ctrl-Y 连续复制不退出
    Copy {
        exit: bool,
    },
    /// 后台复制完成
    CopyFinished {
        exit: bool,
        ok: bool,
    },
    /// 后台 pin/delete 完成，带回重拉后的列表（None = worker 异常，放弃）
    ListReloaded(Option<Vec<store::Clip>>),
    /// 删除/外部变更后的重载：与 ListReloaded 不同——不主动滚动（选中按
    /// id 重定位，行上移自然带动，fzf --track 同款），并抑制悬停跟随
    DeleteReloaded(Option<Vec<store::Clip>>),
    /// 窗口重新聚焦：打开期间 daemon 可能捕获了新内容，重拉列表
    /// （走 DeleteReloaded 路径：不滚动，选中按 id 重定位）
    Refocus,
    CloseRequested,
    /// 鼠标悬停行：跟随选中（高亮预览）
    Hover(usize),
    /// 鼠标真实移动（物理事件，仅订阅层派发）：恢复悬停跟随。
    /// 不能挂 mouse_area.on_move——键盘导航滚动时列表在静止指针下滑过，
    /// 布局重算会给指针下的行派发 on_move/on_enter（非物理移动），
    /// mouse_follow 被重新打开后悬停跟随即抢走键盘选中：快速连按方向键
    /// 时高亮/预览在键盘位置与指针位置间震荡（滚动越远概率越高）
    MouseMove,
    /// 鼠标点击行：定位并复制关闭（对齐 Enter 语义）
    Pick(usize),
    /// 鼠标右键行：定位并连续复制（对齐 Ctrl-Y 语义，不退出）
    PickStay(usize),
    /// 滚动反馈：带上真实视口高度，修正滚动跟随的居中估算
    Scrolled(scrollable::Viewport),
    /// 后台图片解码完成（None = 读取/解码失败，进入 failed 集不再重试）。
    /// 解码在后台线程完成（image crate → RGBA），渲染器拿到的是现成像素，
    /// 不再在 UI 线程同步解码造成帧冻结（切换到未缓存截图时闪烁/卡顿根因）
    ImageReady {
        id: i64,
        handle: Option<image::Handle>,
    },
}

// 键入、搜索结果和列表重载都可能携带剪贴板正文。调试日志只记录事件名。
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Query(_) => "Query",
            Self::SearchDone { .. } => "SearchDone",
            Self::Key(..) => "Key",
            Self::Copy { .. } => "Copy",
            Self::CopyFinished { .. } => "CopyFinished",
            Self::ListReloaded(_) => "ListReloaded",
            Self::DeleteReloaded(_) => "DeleteReloaded",
            Self::Refocus => "Refocus",
            Self::CloseRequested => "CloseRequested",
            Self::Hover(_) => "Hover",
            Self::MouseMove => "MouseMove",
            Self::Pick(_) => "Pick",
            Self::PickStay(_) => "PickStay",
            Self::Scrolled(_) => "Scrolled",
            Self::ImageReady { .. } => "ImageReady",
        })
    }
}

/// 列内固定高度垂直间隙（iced 0.14 的 space::vertical() 是 Fill 语义，
/// 需要定高间隙时用它手动构造）
fn vspace(h: f32) -> space::Space {
    space::Space::new().height(Length::Fixed(h))
}

/// SQLite、复制和图片解码使用按需创建的阻塞线程，键盘事件订阅始终可运行。
/// worker panic 时回传 `on_panic`——由调用方指定兜底消息，保证语义正确
/// （Copy 任务 panic 必须走 CopyFinished{ok:false} 触发失败通知，而非被
/// ListReloaded(None) 静默吞掉）。
fn run_bg<T, F>(
    f: F,
    wrap: impl Fn(T) -> Message + Send + 'static,
    on_panic: Message,
) -> Task<Message>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    Task::perform(
        async move { tokio::task::spawn_blocking(f).await },
        move |result| match result {
            Ok(v) => wrap(v),
            Err(_) => on_panic,
        },
    )
}

/// 过滤结果缓存条目。
///
/// `idxs` 的**下标空间取决于来源**——早期实现漏了这个区分，把 FTS 候选链产出的
/// 下标直接拿去索引 `clips`，造成两个后果：① 搜索结果错位（显示的变成列表前 N 条
/// 而非真正命中，Enter/复制会复制到非命中项）；② `max_items < SEARCH_LIMIT` 时
/// `clips[i]` 越界 panic。故来源必须与下标一起缓存。
#[derive(Clone)]
struct FilteredCache {
    /// `clips` 代数：整表重载即失效（pin/delete/copy 后长度可能不变，不能只比长度）
    gen: u64,
    query: String,
    /// true = `idxs` 指向 `search_hits`（FTS/LIKE 候选链）；false = 指向 `clips`
    from_search: bool,
    idxs: Vec<usize>,
}

/// 计算过滤结果，返回 `(下标是否指向 search_hits, 下标列表)`。
///
/// 三条路径：空查询 = 原序；有新鲜搜索候选 = 相关度重排；其余 = 对已载入窗口
/// 做内存子序列过滤。
///
/// **返回的第一项必须与 `idxs` 一起使用**——`search_hits` 与 `clips` 是顺序和
/// 内容都不同的两个 Vec，拿候选链的下标去索引 `clips` 会取到错误条目，并在候选数
/// 超过列表窗口（`max_items` 设小于 `SEARCH_LIMIT`）时越界 panic（历史缺陷，
/// 已由 `search_hit_indices_resolve_against_search_hits` 等单测锁定）。
fn compute_filtered(
    query: &str,
    clips: &[store::Clip],
    search_hits: &[store::Clip],
    search_hits_query: &str,
    search_hits_gen: u64,
    clips_gen: u64,
) -> (bool, Vec<usize>) {
    if query.trim().is_empty() {
        return (false, (0..clips.len()).collect());
    }
    let q = query.trim().to_lowercase();
    // 候选是子串命中，fuzzy 子序列必命中；极端大小写折叠差异导致全不匹配时
    // 回落搜索自身序（不展示空列表）
    if search_hits_query == query && search_hits_gen == clips_gen && !search_hits.is_empty() {
        let mut scored: Vec<(i32, usize)> = search_hits
            .iter()
            .enumerate()
            .filter_map(|(i, c)| fuzzy_score(&q, &c.text).map(|s| (s, i)))
            .collect();
        if scored.is_empty() {
            return (true, (0..search_hits.len()).collect());
        }
        // 稳定排序：并列保持搜索自身的相关度序
        scored.sort_by_key(|s| Reverse(s.0));
        return (true, scored.into_iter().map(|(_, i)| i).collect());
    }
    let mut scored: Vec<(i32, usize)> = clips
        .iter()
        .enumerate()
        .filter_map(|(i, c)| fuzzy_score(&q, &c.text).map(|s| (s, i)))
        .collect();
    scored.sort_by_key(|s| Reverse(s.0));
    (false, scored.into_iter().map(|(_, i)| i).collect())
}

struct App {
    position: Option<position::PositionMemory>,
    warm_window: Option<iced::window::Id>,
    closing: bool,
    search_id: iced::widget::Id,
    /// 行列表 scrollable 的 Id：键盘导航时 scroll_to 跟随选中
    list_id: iced::widget::Id,
    preview_id: iced::widget::Id,
    /// 搜索会话可以保留查询并把焦点交回列表，Esc 才结束会话。
    search_mode: bool,
    clips: Vec<store::Clip>,
    /// clips 更替代数：每次整表重载 +1，作为过滤缓存的失效键之一
    /// （pin/delete/copy 后列表都会整表重拉，长度可能不变，不能只比长度）
    clips_gen: u64,
    /// FTS 全库搜索候选（任务 2.1，后台线程产出）+ 新鲜度标记：
    /// (query, gen) 双匹配才使用，否则回落内存模糊过滤（无闪烁）
    search_hits: Vec<store::Clip>,
    search_hits_query: String,
    search_hits_gen: u64,
    /// 一次只查一个词。查询结束后再读取输入框的最新内容。
    search_in_flight: bool,
    query: String,
    /// 过滤结果缓存。悬停/选中/复制等高频事件都会调 filtered()，全量评分排序
    /// O(n·m) 不能每事件重算；来源与下标必须成对缓存，见 `FilteredCache`
    filtered_cache: RefCell<Option<FilteredCache>>,
    selected: usize,
    /// 选中项身份（fzf --track 的确定性实现）：clips 只增代数不保序——
    /// store::list 把 ▶ 当前项置顶、星标可置顶，daemon 捕获/其他窗口操作
    /// 都会重排列表。仅靠索引维持选中时，重载后同索引 ≠ 同条目：
    /// 高亮漂移到别的行，Ctrl-X 就会删掉"看起来选中"以外的记录。
    /// 重载后一律按 selected_id 在新列表重新定位（relocate_selected）。
    selected_id: Option<i64>,
    /// 星标条目删除二段确认（对齐路线图 1.5：内嵌确认，去 fuzzel 依赖）
    confirm_delete: bool,
    preview_width: usize,
    /// 图片预览开关（对齐 fzf TUI 的 enable_image_preview 语义）
    image_preview_enabled: bool,
    /// 底部预览窗格开关（enable_preview 配置）
    enable_preview: bool,
    /// 桌面通知开关（复制失败等反馈）
    notify_enabled: bool,
    /// 滚动视口实测半高（on_scroll 回填，resize 自适应），初始为估算值
    viewport_half: f32,
    viewport_offset: f32,
    /// 鼠标悬停跟随开关：键盘导航时关闭——列表在静止指针下滑动会
    /// 逐行触发悬停事件，悬停跟随会反复抢走选中态（界面闪烁根因）
    mouse_follow: bool,
    /// 当前项指针 TTL 缓存：(读取时刻, 值)。▶ 标记的文件读收敛到
    /// 至多 2 次/秒（TTL 内的滞后对 ▶ 展示无感知影响）
    cur_cache: RefCell<Option<(Instant, Option<String>)>>,
    /// 按 clip id 缓存已解码的 RGBA 缩略图。
    /// Handle 来自后台线程预解码（ImageReady），view 只读缓存不碰 IO/解码；
    /// clip id 内容不可变，按 id 缓存安全。view(&self) 下用 RefCell。
    image_cache: RefCell<Vec<(i64, image::Handle)>>,
    /// 同时只解码一张，完成后按当前选中项继续。
    decoding: bool,
    /// 解码失败（文件缺失/格式坏）：不再重试，预览固定显示缺失提示
    decode_failed: std::collections::HashSet<i64>,
}

impl App {
    fn boot(cfg: SelectorConfig) -> (Self, Task<Message>) {
        let position = position::PositionMemory::open(cfg.remember_window_position);
        let clips =
            store::list_with_pinning(Self::load_limit(), cfg.pinned_on_top).unwrap_or_default();
        let mut app = Self::with_history(cfg, clips);
        app.position = position;
        app.refresh_cur();
        let decode = app.ensure_image_decode();
        (app, decode)
    }

    fn with_history(cfg: SelectorConfig, clips: Vec<store::Clip>) -> Self {
        let first_id = clips.first().map(|c| c.id);
        Self {
            position: None,
            warm_window: None,
            closing: false,
            search_id: iced::widget::Id::unique(),
            list_id: iced::widget::Id::unique(),
            preview_id: iced::widget::Id::unique(),
            // 搜索默认关（fzf 语义：/ 才进入）：启动时不聚焦输入框，光标
            // 不闪烁，窗口零重绘待机
            search_mode: false,
            clips,
            clips_gen: 0,
            search_hits: Vec::new(),
            search_hits_query: String::new(),
            search_hits_gen: 0,
            search_in_flight: false,
            query: String::new(),
            filtered_cache: RefCell::new(None),
            selected: 0,
            // 打开即高亮第 1 行（▶ 当前项）：记录其身份供重载后重定位
            selected_id: first_id,
            confirm_delete: false,
            preview_width: cfg.preview_width,
            image_preview_enabled: cfg.enable_image_preview,
            enable_preview: cfg.enable_preview,
            notify_enabled: cfg.notify_enabled,
            viewport_half: VIEWPORT_HALF,
            viewport_offset: 0.0,
            mouse_follow: false,
            cur_cache: RefCell::new(None),
            image_cache: RefCell::new(Vec::new()),
            decoding: false,
            decode_failed: std::collections::HashSet::new(),
        }
    }

    /// 列表窗口上限：与 fzf TUI 取同一常量（`store::MENU_LIMIT`），
    /// 保证两个后端看到的条目数一致。搜索面不受此窗口限制——任何非空查询
    /// 都走 `store::search`（≥3 字符 FTS / <3 字符 LIKE 全库回退）
    fn load_limit() -> usize {
        store::MENU_LIMIT
    }

    /// 短查询也查全库。保留原始输入作请求标识，空白由 store::search 统一裁剪。
    fn request_search(&mut self) -> Task<Message> {
        if self.search_in_flight
            || self.query.trim().is_empty()
            || (self.search_hits_query == self.query && self.search_hits_gen == self.clips_gen)
        {
            return Task::none();
        }
        self.search_in_flight = true;
        let q_bg = self.query.clone();
        let gen = self.clips_gen;
        // query/gen 打包进 T 侧返回值：run_bg 的 wrap 是 Fn，不能捕获移动
        // 外部变量（iced Task::perform 的包装闭包可能被多次调用）
        run_bg(
            move || match store::search(&q_bg, store::SEARCH_LIMIT) {
                Ok(hits) => (q_bg, gen, hits),
                Err(_) => (q_bg, gen, Vec::new()),
            },
            |(query, gen, hits)| Message::SearchDone { query, gen, hits },
            Message::SearchDone {
                query: self.query.clone(),
                gen,
                hits: Vec::new(),
            },
        )
    }

    /// 过滤后的视图：空查询=存储序；有查询时优先用后台线程产出的
    /// FTS/LIKE 候选链（覆盖全库），否则回落内存子序列过滤。
    /// 结果按 (clips 代数, 查询) 缓存——同一输入下悬停/选中/复制等
    /// 事件重复调用直接复用，不在事件路径上重算全库评分
    fn filtered(&self) -> Vec<&store::Clip> {
        // 缓存命中：按下标取（连来源一起取回，见 FilteredCache）
        {
            let cache = self.filtered_cache.borrow();
            if let Some(c) = cache.as_ref() {
                if c.gen == self.clips_gen && c.query == self.query {
                    let source = self.source_of(c.from_search);
                    return c.idxs.iter().filter_map(|&i| source.get(i)).collect();
                }
            }
        }
        let (from_search, idxs) = self.compute_filtered();
        let source = self.source_of(from_search);
        // 用 `.get` 而非 `[]`：下标空间一旦再有偏差，宁缺勿 panic
        // （历史缺陷：FTS 下标被拿去索引 clips，见 FilteredCache 注释）
        let out = idxs.iter().filter_map(|&i| source.get(i)).collect();
        *self.filtered_cache.borrow_mut() = Some(FilteredCache {
            gen: self.clips_gen,
            query: self.query.clone(),
            from_search,
            idxs,
        });
        out
    }

    /// 下标空间对应的数据源
    fn source_of(&self, from_search: bool) -> &[store::Clip] {
        if from_search {
            &self.search_hits
        } else {
            &self.clips
        }
    }

    /// 计算过滤结果，委托给纯函数 `compute_filtered`（便于单测）
    fn compute_filtered(&self) -> (bool, Vec<usize>) {
        compute_filtered(
            &self.query,
            &self.clips,
            &self.search_hits,
            &self.search_hits_query,
            self.search_hits_gen,
            self.clips_gen,
        )
    }

    /// 可渲染行数：过滤结果与列表窗口取小。选中态/快选/导航必须以此为界——
    /// 只画前 `MENU_LIMIT` 行，越界的高亮会落在不存在的行上
    fn visible_len(&self) -> usize {
        self.filtered().len().min(store::MENU_LIMIT)
    }

    /// 设置选中（索引 + 身份同步）。所有选中变更的唯一入口：
    /// 索引与 selected_id 永远成对更新，Ctrl-X/Copy 取的是高亮行本身
    fn set_selection(&mut self, idx: usize) {
        self.selected = idx;
        let id = self.filtered().get(idx).map(|c| c.id);
        self.selected_id = id;
    }

    /// 重载后重定位选中（fzf --track 的确定性实现）：
    /// 1) selected_id 仍在 → 高亮跟随该条目。防重排漂移：store::list 依赖
    ///    current 指针/星标排序，daemon 捕获、Ctrl-P 固定都会重排行序，
    ///    同索引 ≠ 同条目——索引维持会把高亮留在别的行，Ctrl-X 即删错行；
    /// 2) id 消失（已被删）→ 保留索引让下一行自然顶上，末行回退一位；
    ///    随后回写当前索引处的 id，保证一致性不变式成立
    fn relocate_selected(&mut self) {
        let n = self.visible_len();
        if n == 0 {
            self.selected = 0;
            self.selected_id = None;
            return;
        }
        if let Some(id) = self.selected_id {
            let found = self.filtered().iter().position(|c| c.id == id);
            if let Some(idx) = found {
                if idx < n {
                    self.selected = idx;
                    return;
                }
            }
        }
        if self.selected >= n {
            self.selected = n - 1;
        }
        let id = self.filtered().get(self.selected).map(|c| c.id);
        self.selected_id = id;
    }

    fn subscription(&self) -> Subscription<Message> {
        event::listen_with(|event, status, _| Self::input_event(event, status))
    }

    fn input_event(event: iced::Event, status: event::Status) -> Option<Message> {
        match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                modifiers,
                repeat,
                ..
            }) => {
                use keyboard::key::Named;

                let captured = status == event::Status::Captured;
                match key.as_ref() {
                    // 搜索框消费了剪切时，不再把同一次按键交给删除。
                    // 长按产生的重复事件也不能充当第二次确认。
                    keyboard::Key::Character("x")
                        if modifiers.control() && (captured || repeat) =>
                    {
                        return None;
                    }
                    keyboard::Key::Named(Named::Tab) if repeat => return None,
                    // 保留输入框对文本与光标移动的处理。
                    keyboard::Key::Character("/")
                    | keyboard::Key::Named(Named::Home | Named::End)
                        if captured =>
                    {
                        return None;
                    }
                    _ => {}
                }
                Some(Message::Key(key, modifiers))
            }
            // 悬停跟随只由物理指针移动恢复（见 Message::MouseMove 注释）
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => Some(Message::MouseMove),
            // 重新聚焦即重拉列表：窗口开着的时候 daemon 照常捕获，
            // 不刷新则列表陈旧（看不到新条目、▶ 指针滞后）
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Refocus),
            iced::Event::Window(iced::window::Event::CloseRequested) => {
                Some(Message::CloseRequested)
            }
            _ => None,
        }
    }
}

// 布局常量（视图结构相关，主题配色见 theme.rs）

/// 行定高：Rich 文本 14px × 行高 1.3 ≈ 18.2 + 上下 padding 8，凑整 27。
/// 分界线 1px → 行距（pitch）恒定 28，键盘导航的 scroll_to 据此精确计算
const ROW_HEIGHT: f32 = 27.0;
const ROW_SEPARATOR: f32 = 1.0;
const ROW_PITCH: f32 = ROW_HEIGHT + ROW_SEPARATOR;
/// 视口半高初始估算（675 - 头行/提示符/预览窗格），on_scroll 回填实测值
const VIEWPORT_HALF: f32 = 240.0;
/// 保留最近解码的预览缩略图。
const IMAGE_CACHE_CAP: usize = 4;
/// 底部预览窗格定高（内部 scrollable，长文可滚）
const PREVIEW_HEIGHT: f32 = 220.0;

fn theme_dark(_state: &App) -> iced::Theme {
    iced::Theme::Dark
}

fn main() -> iced::Result {
    // 单实例守卫必须存活到进程结束（drop 即释放 flock）
    let instance = ensure_single_instance();
    // 提前初始化 iced 的共享字体库，与 Wayland 和历史初始化并行。
    // 工作线程完成后退出；若未能启动，iced 会在首次排版时照常初始化。
    let _ = std::thread::Builder::new().spawn(|| {
        let _ = iced::advanced::graphics::text::font_system();
    });
    let cfg = SelectorConfig::load();
    if cfg.gui_idle_timeout_secs > 0 {
        if let Some(guard) = &instance {
            match niri_clip_core::gui_activation::Endpoint::bind(guard) {
                Ok((_endpoint, listener)) => return warm::run(cfg, listener),
                Err(e) => eprintln!("[niri-clip gui] standby unavailable: {e}"),
            }
        }
    }
    // 常规 xdg 窗口：受 niri window-rule 约束（悬浮/位置/边框由用户
    // rule.kdl 约定，app-id = "niri-clip-gui"），winit 原生 IME
    iced::application(move || App::boot(cfg.clone()), App::update, App::view)
        .executor::<executor::GuiExecutor>()
        .title("niri-clip")
        // 深色主题（光标/默认控件色）+ JetBrainsMono NF：符号字形齐全无 tofu
        .theme(theme_dark)
        .default_font(UI_FONT)
        .subscription(App::subscription)
        .window(window_settings())
        .run()
}

fn window_settings() -> iced::window::Settings {
    iced::window::Settings {
        size: iced::Size::new(500.0, 675.0),
        resizable: true,
        exit_on_close_request: false,
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: String::from("niri-clip-gui"),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use niri_clip_core::config::Config;

    fn clip(id: i64, text: &str) -> store::Clip {
        store::Clip {
            id,
            hash: format!("h{id}"),
            text: text.to_string(),
            mime: "text/plain".to_string(),
            pinned: false,
            image_path: None,
        }
    }

    /// 按下标取条目（与 `App::filtered()` 同口径：越界宁缺勿 panic）
    fn pick(source: &[store::Clip], idxs: &[usize]) -> Vec<i64> {
        idxs.iter()
            .filter_map(|&i| source.get(i))
            .map(|c| c.id)
            .collect()
    }

    /// 回归锁定（真实缺陷）：搜索候选链的下标必须按 `search_hits` 解释。
    /// 历史实现把 `search_hits` 的下标拿去索引 `clips`（两者顺序与内容都不同）
    /// → 显示成列表前 N 条而非真正命中，Enter/复制会作用到非命中项。
    #[test]
    fn search_hit_indices_resolve_against_search_hits() {
        let clips = vec![clip(1, "alpha"), clip(2, "bravo"), clip(3, "charlie")];
        // 顺序与 clips 相反：命中 bravo 时若按 clips 解释会取到 alpha
        let hits = vec![clip(2, "bravo"), clip(3, "charlie")];
        let (from_search, idxs) = compute_filtered("ra", &clips, &hits, "ra", 0, 0);
        assert!(from_search, "有新鲜候选时必须走候选链");
        assert_eq!(
            pick(&hits, &idxs),
            vec![2],
            "应命中 bravo；若按 clips 解释会错取 alpha（历史缺陷）"
        );
    }

    /// 候选数超过列表窗口（`max_items` < `SEARCH_LIMIT`）时不得越界
    #[test]
    fn search_hits_longer_than_window_does_not_panic() {
        let clips = vec![clip(1, "aaa")]; // 列表窗口只有 1 条
        let hits = vec![clip(9, "aaa"), clip(8, "aab"), clip(7, "aac")];
        let (from_search, idxs) = compute_filtered("aa", &clips, &hits, "aa", 0, 0);
        assert!(from_search);
        assert!(
            idxs.iter().all(|&i| i < hits.len()),
            "下标必须落在候选集内：{idxs:?}"
        );
        assert_eq!(pick(&hits, &idxs).len(), 3);
    }

    /// 候选过期（代数不符）时必须回落列表窗口，不得使用陈旧命中
    #[test]
    fn stale_search_hits_fall_back_to_loaded_window() {
        let clips = vec![clip(1, "alpha"), clip(2, "bravo")];
        let hits = vec![clip(3, "charlie")];
        let (from_search, idxs) = compute_filtered("ra", &clips, &hits, "ra", 7, 9);
        assert!(!from_search, "代数不符的候选不得使用");
        assert_eq!(pick(&clips, &idxs), vec![2]);
    }

    /// 空查询 = 原序全窗口
    #[test]
    fn empty_query_returns_full_window_in_order() {
        let clips = vec![clip(1, "a"), clip(2, "b"), clip(3, "c")];
        let (from_search, idxs) = compute_filtered("", &clips, &[], "", 0, 0);
        assert!(!from_search);
        assert_eq!(pick(&clips, &idxs), vec![1, 2, 3]);
    }

    #[test]
    fn completed_search_replaces_the_window_cache_and_updates_selection() {
        let mut app = App::with_history(Config::default().into(), vec![clip(1, "recent")]);
        let _ = app.handle_message(Message::Query(" older ".into()));
        assert!(app.filtered().is_empty());
        let _ = app.handle_message(Message::SearchDone {
            query: " older ".into(),
            gen: 0,
            hits: vec![clip(2, "older entry")],
        });
        assert_eq!(app.filtered()[app.selected].id, 2);
        assert_eq!(app.selected_id, Some(2));
        assert!(!app.search_in_flight);
    }

    #[test]
    fn rapid_input_only_dispatches_the_latest_waiting_query() {
        let mut app = App::with_history(Config::default().into(), vec![]);
        let _ = app.handle_message(Message::Query("a".into()));
        app.query = "ab".into();
        assert_eq!(app.request_search().units(), 0);
        app.query = "abc".into();
        assert_eq!(app.request_search().units(), 0);
        let next = app.handle_message(Message::SearchDone {
            query: "a".into(),
            gen: 0,
            hits: vec![clip(1, "a")],
        });
        assert_eq!(next.units(), 1);
        assert!(app.search_hits.is_empty());
        assert!(app.search_in_flight);
    }

    #[test]
    fn image_navigation_decodes_only_one_selection_at_a_time() {
        let clips = (1..=3)
            .map(|id| store::Clip {
                mime: "image/png".into(),
                image_path: Some(format!("{id}.bin")),
                ..clip(id, "image")
            })
            .collect();
        let cfg = Config {
            enable_image_preview: true,
            ..Default::default()
        };
        let mut app = App::with_history(cfg.into(), clips);
        assert_eq!(app.ensure_image_decode().units(), 1);
        app.set_selection(1);
        assert_eq!(app.ensure_image_decode().units(), 0);
        app.set_selection(2);
        let _ = app.handle_message(Message::ImageReady {
            id: 1,
            handle: None,
        });
        assert_eq!(app.ensure_image_decode().units(), 1);
        assert_eq!(app.selected_id, Some(3));
    }

    #[test]
    fn ctrl_x_only_reaches_delete_for_a_fresh_unclaimed_press() {
        let route = |status, repeat| {
            let key = keyboard::Key::Character("x".into());
            App::input_event(
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    modified_key: key.clone(),
                    key,
                    physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyX),
                    location: keyboard::Location::Standard,
                    modifiers: keyboard::Modifiers::CTRL,
                    text: None,
                    repeat,
                }),
                status,
            )
        };
        assert!(route(event::Status::Captured, false).is_none());
        assert!(route(event::Status::Ignored, true).is_none());
        assert!(matches!(
            route(event::Status::Ignored, false),
            Some(Message::Key(_, _))
        ));
    }

    #[test]
    fn preview_scroll_preserves_selection_while_page_navigation_changes_it() {
        use keyboard::key::Named;

        let clips = (1..=40).map(|id| clip(id, "synthetic")).collect();
        let mut app = App::with_history(Config::default().into(), clips);
        app.viewport_half = 2.0 * ROW_PITCH;

        let _ = app.on_key(
            keyboard::Key::Named(Named::PageDown),
            keyboard::Modifiers::empty(),
        );
        assert_eq!(app.selected_id, Some(5));
        let scroll = app.on_key(
            keyboard::Key::Named(Named::PageDown),
            keyboard::Modifiers::SHIFT,
        );
        assert_eq!(scroll.units(), 1);
        assert_eq!(app.selected_id, Some(5));
        let _ = app.on_key(
            keyboard::Key::Named(Named::End),
            keyboard::Modifiers::empty(),
        );
        assert_eq!(app.selected_id, Some(40));
        let _ = app.on_key(
            keyboard::Key::Named(Named::Home),
            keyboard::Modifiers::empty(),
        );
        assert_eq!(app.selected_id, Some(1));
    }

    #[test]
    fn debug_events_omit_keys_queries_and_history() {
        let marker = "synthetic-private-text";
        let hits = vec![clip(1, marker)];
        for event in [
            Message::Key(
                keyboard::Key::Character(marker.into()),
                keyboard::Modifiers::empty(),
            ),
            Message::Query(marker.into()),
            Message::SearchDone {
                query: marker.into(),
                gen: 0,
                hits: hits.clone(),
            },
            Message::ListReloaded(Some(hits.clone())),
            Message::DeleteReloaded(Some(hits)),
        ] {
            assert!(!format!("{event:?}").contains(marker));
        }
    }
}
