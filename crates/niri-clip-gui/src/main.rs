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

mod image_cache;
mod instance;
mod search;
mod theme;
mod update;
mod view;

use instance::ensure_single_instance;
use search::{fuzzy_flags, fuzzy_score};
use theme::*;

use iced::widget::{
    container, image, mouse_area, operation, row, rule, scrollable, space, text, text_input,
};
use iced::{
    keyboard, mouse, Background, Border, Element, Font, Length, Shadow, Subscription, Task,
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
    Copy { exit: bool },
    /// 后台复制完成
    CopyFinished { exit: bool, ok: bool },
    /// 后台 pin/delete 完成，带回重拉后的列表（None = worker 异常，放弃）
    ListReloaded(Option<Vec<store::Clip>>),
    /// 删除/外部变更后的重载：与 ListReloaded 不同——不主动滚动（选中按
    /// id 重定位，行上移自然带动，fzf --track 同款），并抑制悬停跟随
    DeleteReloaded(Option<Vec<store::Clip>>),
    /// 窗口重新聚焦：打开期间 daemon 可能捕获了新内容，重拉列表
    /// （走 DeleteReloaded 路径：不滚动，选中按 id 重定位）
    Refocus,
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
    /// 启动后触发一次：让首屏选中的图片也走后台解码路径
    Tick,
}

/// 列内固定高度垂直间隙（iced 0.14 的 space::vertical() 是 Fill 语义，
/// 需要定高间隙时用它手动构造）
fn vspace(h: f32) -> space::Space {
    space::Space::new().height(Length::Fixed(h))
}

/// 把阻塞任务丢到后台线程执行（iced 默认执行器为 thread-pool，
/// Task::perform 的 future 里阻塞仅占一个 worker，UI 线程不受影响）。
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
        async move {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(f());
            });
            rx.recv().ok()
        },
        move |t| match t {
            Some(v) => wrap(v),
            None => on_panic,
        },
    )
}

struct App {
    search_id: iced::widget::Id,
    /// 行列表 scrollable 的 Id：键盘导航时 scroll_to 跟随选中
    list_id: iced::widget::Id,
    /// 搜索模式（fzf 语义：/ 或 Ctrl-F 才进入，Esc 退出）。未进入时输入框
    /// 不持有焦点——光标不闪烁 → 窗口零强制重绘待机（闪烁的最后来源）。
    /// 退出时轮换 search_id 使旧 widget 状态连同焦点一起丢弃
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
    query: String,
    /// 过滤结果缓存：(代数, 查询, 命中的 clips 下标)。悬停/选中/复制等
    /// 高频事件都会调 filtered()，750 条全量评分排序 O(n·m) 不能每事件重算
    filtered_cache: RefCell<Option<(u64, String, Vec<usize>)>>,
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
    /// 鼠标悬停跟随开关：键盘导航时关闭——列表在静止指针下滑动会
    /// 逐行触发 on_enter，悬停跟随会反复抢走选中态（界面闪烁根因）
    mouse_follow: bool,
    /// 当前项指针 TTL 缓存：(读取时刻, 值)。▶ 标记的文件读收敛到
    /// 至多 2 次/秒（TTL 内的滞后对 ▶ 展示无感知影响）
    cur_cache: RefCell<Option<(Instant, Option<String>)>>,
    /// 图片 Handle LRU 缓存（clip id → Handle，已解码 RGBA）。
    /// Handle 来自后台线程预解码（ImageReady），view 只读缓存不碰 IO/解码；
    /// clip id 内容不可变，按 id 缓存安全。view(&self) 下用 RefCell。
    image_cache: RefCell<Vec<(i64, image::Handle)>>,
    /// 正在后台解码的图片 id（防重复派发）
    decoding: std::collections::HashSet<i64>,
    /// 解码失败（文件缺失/格式坏）：不再重试，预览固定显示缺失提示
    decode_failed: std::collections::HashSet<i64>,
}

impl App {
    fn new() -> Self {
        let cfg = config::Config::load();
        let clips = store::list(Self::load_limit()).unwrap_or_default();
        let first_id = clips.first().map(|c| c.id);
        Self {
            search_id: iced::widget::Id::unique(),
            list_id: iced::widget::Id::unique(),
            // 搜索默认关（fzf 语义：/ 才进入）：启动时不聚焦输入框，光标
            // 不闪烁，窗口零重绘待机
            search_mode: false,
            clips,
            clips_gen: 0,
            search_hits: Vec::new(),
            search_hits_query: String::new(),
            search_hits_gen: 0,
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
            mouse_follow: false,
            cur_cache: RefCell::new(None),
            image_cache: RefCell::new(Vec::new()),
            decoding: std::collections::HashSet::new(),
            decode_failed: std::collections::HashSet::new(),
        }
    }

    fn load_limit() -> usize {
        // 全量载入（DB 本身受 max_items 约束）：搜索范围不再止于旧
        // TUI_LIMIT=300，渲染侧另有 MAX_RENDER_ROWS 兜底
        let cfg = config::Config::load();
        cfg.max_items
    }

    /// FTS 全库搜索请求（任务 2.1）：查询 ≥3 字符才走 trigram MATCH
    ///（trigram 索引对短查询无增益，回落内存模糊过滤）。发出即失效
    /// 旧结果（回落内存过滤，不展示过期候选）；worker panic 静默空集
    fn request_search(&mut self) -> Task<Message> {
        if self.query.trim().chars().count() < 3 {
            self.search_hits_query.clear();
            return Task::none();
        }
        if self.search_hits_query == self.query && self.search_hits_gen == self.clips_gen {
            return Task::none();
        }
        self.search_hits_query.clear();
        let q_bg = self.query.trim().to_owned();
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
                query: String::new(),
                gen: 0,
                hits: Vec::new(),
            },
        )
    }

    /// 过滤后的视图：fzf 风格子序列匹配 + 简易评分排序
    /// （连续命中/词首加权，命中越早越好）；空查询保持存储序。
    /// 结果按 (clips 代数, 查询) 缓存——同一输入下悬停/选中/复制等
    /// 事件重复调用直接复用，不在事件路径上重算全库评分
    fn filtered(&self) -> Vec<&store::Clip> {
        {
            let cache = self.filtered_cache.borrow();
            if let Some((gen, q, idxs)) = cache.as_ref() {
                if *gen == self.clips_gen && *q == self.query {
                    return idxs.iter().map(|&i| &self.clips[i]).collect();
                }
            }
        }
        let idxs: Vec<usize> = if self.query.is_empty() {
            (0..self.clips.len()).collect()
        } else if self.search_hits_query == self.query
            && self.search_hits_gen == self.clips_gen
            && !self.search_hits.is_empty()
        {
            // FTS 全库候选（后台线程产出，任务 2.1）+ fzf 风格评分重排，
            // 与内存过滤同 UX。候选是子串命中，fuzzy 子序列必命中；
            // 极端大小写折叠差异导致全部不匹配时回落 FTS 相关度序
            let q = self.query.to_lowercase();
            let mut scored: Vec<(i32, usize)> = self
                .search_hits
                .iter()
                .enumerate()
                .filter_map(|(i, c)| fuzzy_score(&q, &c.text).map(|s| (s, i)))
                .collect();
            if scored.is_empty() {
                (0..self.search_hits.len()).collect()
            } else {
                // 稳定排序：并列保持 FTS bm25 相关度序
                scored.sort_by_key(|s| Reverse(s.0));
                scored.into_iter().map(|(_, i)| i).collect()
            }
        } else {
            let q = self.query.to_lowercase();
            let mut scored: Vec<(i32, usize)> = self
                .clips
                .iter()
                .enumerate()
                .filter_map(|(i, c)| fuzzy_score(&q, &c.text).map(|s| (s, i)))
                .collect();
            scored.sort_by_key(|s| Reverse(s.0));
            scored.into_iter().map(|(_, i)| i).collect()
        };
        *self.filtered_cache.borrow_mut() =
            Some((self.clips_gen, self.query.clone(), idxs.clone()));
        idxs.into_iter().map(|i| &self.clips[i]).collect()
    }

    /// 可渲染行数：过滤结果与渲染上限取小。选中态/快选/导航必须以此为界——
    /// 只画前 MAX_RENDER_ROWS 行，越界的高亮会落在不存在的行上
    fn visible_len(&self) -> usize {
        self.filtered().len().min(MAX_RENDER_ROWS)
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
        iced::event::listen_with(|event, _status, _id| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                Some(Message::Key(key, modifiers))
            }
            // 悬停跟随只由物理指针移动恢复（见 Message::MouseMove 注释）
            iced::Event::Mouse(mouse::Event::CursorMoved { .. }) => Some(Message::MouseMove),
            // 重新聚焦即重拉列表：窗口开着的时候 daemon 照常捕获，
            // 不刷新则列表陈旧（看不到新条目、▶ 指针滞后）
            iced::Event::Window(iced::window::Event::Focused) => Some(Message::Refocus),
            _ => None,
        })
    }
}

// 布局常量（视图结构相关，主题配色见 theme.rs）

/// 行定高：Rich 文本 14px × 行高 1.3 ≈ 18.2 + 上下 padding 8，凑整 27。
/// 分界线 1px → 行距（pitch）恒定 28，键盘导航的 scroll_to 据此精确计算
const ROW_HEIGHT: f32 = 27.0;
const ROW_PITCH: f32 = ROW_HEIGHT + 1.0;
/// 视口半高初始估算（675 - 头行/提示符/预览窗格），on_scroll 回填实测值
const VIEWPORT_HALF: f32 = 240.0;
/// 渲染行数上限：全库载入后（max_items 可到 750）布局成本兜底；
/// 过滤命中超过上限时只渲染相关度最高的前 300 行
const MAX_RENDER_ROWS: usize = 300;
/// 图片 Handle LRU 上限：缓存的是已解码 RGBA（1080p 截图 ≈8MB/张），
/// 4 张约 32MB 上限——内存预算与回滚免解码体验的折中
const IMAGE_CACHE_CAP: usize = 4;
/// 底部预览窗格定高（内部 scrollable，长文可滚）
const PREVIEW_HEIGHT: f32 = 220.0;

fn theme_dark(_state: &App) -> iced::Theme {
    iced::Theme::Dark
}

fn main() -> iced::Result {
    ensure_single_instance();
    // 常规 xdg 窗口：受 niri window-rule 约束（悬浮/位置/边框由用户
    // rule.kdl 约定，app-id = "niri-clip-gui"），winit 原生 IME
    iced::application(
        || {
            let app = App::new();
            // 搜索默认关（/ 才进入）：启动不聚焦输入框——光标不闪烁，窗口
            // 零强制重绘待机。Tick 触发首屏选中图片的后台解码派发
            // （ensure_image_decode 挂在 update 尾部，启动后需一条消息引出）
            let tick = Task::perform(async {}, move |_| Message::Tick);
            (app, tick)
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
