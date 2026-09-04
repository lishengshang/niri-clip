//! 渲染（自 main.rs 拆出，纯代码搬移）：主视图 + 底部预览窗格 +
//! ▶/图片缓存读取（缓存刷新 refresh_cur 在 update.rs 消息路径）。

use super::*;

impl App {
    pub(super) fn view(&self) -> Element<'_, Message> {
        let cur = self.cur_hash();
        let filtered = self.filtered();
        let q_lower = self.query.to_lowercase();

        // 视觉对齐 fzf TUI：info 头行 → "剪贴板> " 提示符 → 行列表 → 底部预览
        let counter = format!("{}/{}", filtered.len(), self.clips.len());
        // 提示行：键位 accent 高亮、说明 muted，紧凑单行（fzf header 风格）
        let mut hints: Vec<text::Span<'static, (), Font>> = Vec::new();
        let hint_groups = [
            ("1-9,0", "快选"),
            ("⏎", "复制"),
            ("右键", "连复"),
            ("/", "搜索"),
            ("Ctrl-P", "固定"),
            ("Ctrl-X", "删除"),
            ("Esc", "清除/退出"),
        ];
        for (i, (k, d)) in hint_groups.iter().enumerate() {
            if i > 0 {
                hints.push(text::Span::new(" · ").color(SCROLLBAR));
            }
            hints.push(text::Span::new((*k).to_string()).color(ACCENT));
            hints.push(text::Span::new((*d).to_string()).color(MUTED));
        }

        let rows = filtered
            .iter()
            .enumerate()
            .take(MAX_RENDER_ROWS)
            .map(|(idx, clip)| {
                let selected = idx == self.selected;
                let cursor = if selected { "❯" } else { " " };
                let cur_mark = if cur.as_deref() == Some(clip.hash.as_str()) {
                    "▶"
                } else {
                    " "
                };
                let star = if clip.pinned { "◆" } else { " " };
                // 快选序号任意状态都显示：搜索态裸数字被输入框接管，
                // 序号对应 Alt+1-9,0（见 update.rs 快选分支，0 = 第 10 行）
                let quick = match idx {
                    0..=8 => format!("{}", idx + 1),
                    9 => "0".to_string(),
                    _ => " ".to_string(),
                };
                let prefix = format!("{cursor} {quick} {cur_mark}{star} ");
                // ↵（U+21B5）字形覆盖差（tofu），GUI 侧换成 ⏎
                let preview = preview::preview_text(clip, self.preview_width).replace('↵', "⏎");
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
                    let hit = flags
                        .as_ref()
                        .is_some_and(|f| f.get(i).copied().unwrap_or(false));
                    if i > 0 && hit != run_hit {
                        spans.push(text::Span::new(std::mem::take(&mut run)).color(if run_hit {
                            HL
                        } else {
                            base
                        }));
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
                        shadow: if selected {
                            SHADOW_ROW
                        } else {
                            Shadow::default()
                        },
                        ..Default::default()
                    }),
                )
                .on_press(Message::Pick(idx))
                .on_right_press(Message::PickStay(idx))
                .on_enter(Message::Hover(idx));

                Element::from(row)
            });

        // 行列表：行间细分界线（fzf 行分隔观感）
        let mut list = iced::widget::column![];
        for (idx, row) in rows.enumerate() {
            if idx > 0 {
                list = list.push(rule::horizontal(1.0).style(rule_style));
            }
            list = list.push(row);
        }

        let mut col = iced::widget::column![].push(
            container(
                row![
                    text::Rich::with_spans(hints)
                        .size(10)
                        .font(UI_FONT)
                        .wrapping(text::Wrapping::None),
                    space::horizontal(),
                    text(counter).size(10).color(MUTED)
                ]
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .padding([6, 10]),
        );
        // 列不再用统一 spacing(6)：列表与预览之间零缝隙，预览背景全铺满
        // 到底边；上方元素间的间距改为显式垂直空隙
        col = col
            .push(vspace(6.0))
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
            .push(vspace(6.0))
            .push(rule::horizontal(1.0).style(rule_style))
            .push(vspace(6.0))
            .push(
                scrollable(list)
                    .id(self.list_id.clone())
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .on_scroll(Message::Scrolled)
                    .style(scroll_style),
            );

        if self.confirm_delete {
            col = col.push(vspace(6.0)).push(
                container(text("◆ 星标条目删除确认：再按 Ctrl-X 执行，Esc 取消").size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(confirm_style),
            );
        }

        // 底部预览窗格：文本多行截断；图片条目直接渲染（iced image widget）。
        // 三种分支（文本/图片/缺失提示）外层一律定高 PREVIEW_HEIGHT——高度随
        // 内容变化会让上方 Fill 列表重排，切换条目时整窗闪烁跳动
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
        if self.enable_preview && clip.mime.starts_with("image/") && self.image_preview_enabled {
            match self.image_handle(clip) {
                Some(handle) => {
                    col = col.push(
                        container(image(handle).width(Length::Fill).height(Length::Fill))
                            .width(Length::Fill)
                            .height(Length::Fixed(PREVIEW_HEIGHT))
                            .padding([6, 8])
                            .style(preview_style),
                    );
                }
                None => {
                    // 解码中/失败的占位：面板定高不变，不产生布局跳动
                    let hint = if self.decoding.contains(&clip.id) {
                        format!("[image {}] 解码中…", clip.mime)
                    } else {
                        format!("[image {}] 数据文件缺失或无法解码", clip.mime)
                    };
                    col = col.push(
                        container(text(hint).size(12))
                            .width(Length::Fill)
                            .height(Length::Fixed(PREVIEW_HEIGHT))
                            .padding([6, 8])
                            .style(preview_style),
                    );
                }
            }
        } else if self.enable_preview {
            // 可滚预览窗格：定高小窗 + 内部滚动，长文不再截断丢失
            col = col.push(
                container(
                    scrollable(text(self.preview_text(clip)).size(12))
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .style(scroll_style),
                )
                .width(Length::Fill)
                .height(Length::Fixed(PREVIEW_HEIGHT))
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

    /// ▶ 标记取值：只读缓存，绝不做 IO（IO 由 refresh_cur 在消息路径完成）
    fn cur_hash(&self) -> Option<String> {
        self.cur_cache
            .borrow()
            .as_ref()
            .and_then(|(_, v)| v.clone())
    }

    /// 底部预览：可滚窗格内容（80 行 / 每行 300 字符上限，超出补 …）。
    /// ↵ → ⏎，同行的 tofu 规避
    fn preview_text(&self, clip: &store::Clip) -> String {
        let mut out = String::new();
        for line in clip.text.lines().take(80) {
            let l: String = line.chars().take(300).collect();
            out.push_str(&l);
            out.push('\n');
        }
        if clip.text.len() > out.len() {
            out.push('…');
        }
        out.replace('↵', "⏎")
    }

    /// 图片条目的渲染 Handle：只查缓存（解码由后台线程完成后经
    /// ImageReady 入缓存）。未命中返回 None，view 显示占位提示。
    /// 旧实现 Handle::from_path 按扩展名猜格式（`.bin` 猜不出）→
    /// tiny-skia 渲染线程 panic "Image should be allocated"；
    /// 后改 view 内同步读文件+让渲染器同步解码，卡 UI 闪烁（见
    /// ensure_image_decode 注释），现为纯缓存读
    fn image_handle(&self, clip: &store::Clip) -> Option<image::Handle> {
        self.image_cache
            .borrow()
            .iter()
            .find(|(cid, _)| *cid == clip.id)
            .map(|(_, h)| h.clone())
    }
}
