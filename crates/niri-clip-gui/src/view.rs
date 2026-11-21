//! 渲染（自 main.rs 拆出，纯代码搬移）：主视图 + 底部预览窗格 +
//! ▶/图片缓存读取（缓存刷新 refresh_cur 在 update.rs 消息路径）。

use std::ops::Range;

use iced::widget::{responsive, Column};

use super::*;

/// 列表行预览的列宽预算（显示列，CJK/emoji 计 2），与窗口像素的换算口径：
/// 窗口 500 逻辑 px − 行 padding 左右 20 − 垂直滚动条 ~12 − 前缀 9 字符
/// （JetBrainsMono @14px 的 advance ≈ 8.4px，9×8.4≈75.6）≈ 392px，
/// ÷8.4 ≈ 46 列，取 44 留渲染余量。宁紧勿松：估松了 iced 会在行尾像素级
/// 硬裁（省略号被裁、字切半——正是本常量要修的"生硬截断"），估紧了只是
/// 省略号早现一格。`cfg.preview_width` 仍作用户上限（配小生效）。
const PREVIEW_COLS: usize = 44;

const OVERSCAN_ROWS: usize = 3;

fn history_height(rows: usize) -> f32 {
    (rows as f32 * ROW_PITCH - ROW_SEPARATOR).max(0.0)
}

fn history_window(rows: usize, offset: f32, height: f32) -> Range<usize> {
    let full_height = history_height(rows);
    let height = height.clamp(0.0, full_height);
    // 列表变短或窗口变高时，Scrollable 会把偏移收回新末尾。
    // 先用同样的边界算首屏，避免漏画收回后应出现的行。
    let offset = offset.clamp(0.0, full_height - height);
    let first = (offset / ROW_PITCH).floor() as usize;
    let end = ((offset + height) / ROW_PITCH).ceil() as usize;
    first.saturating_sub(OVERSCAN_ROWS)..(end + OVERSCAN_ROWS).min(rows)
}

impl App {
    fn history_list(
        &self,
        clips: &[&store::Clip],
        current: Option<&str>,
        height: f32,
    ) -> Element<'_, Message> {
        let window = history_window(clips.len(), self.viewport_offset, height);
        let q_lower = self.query.trim().to_lowercase();
        let rows = window.clone().map(|idx| {
            let clip = clips[idx];
            let selected = idx == self.selected;
            let cursor = if selected { "❯" } else { " " };
            let cur_mark = if current == Some(clip.hash.as_str()) {
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
            // cur_mark 与 star 之间必须留空格：▶◆ 直接拼接时两个图标
            // 贴死（当前项恰被固定是最常见组合），视觉上糊成一个字形
            let prefix = format!("{cursor} {quick} {cur_mark} {star} ");
            // ↵（U+21B5）字形覆盖差（tofu），GUI 侧换成 ⏎；截断按显示
            // 列宽（PREVIEW_COLS 口径），cfg.preview_width 作为用户上限
            let preview = preview::preview_text_columns(clip, self.preview_width.min(PREVIEW_COLS))
                .replace('↵', "⏎");
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
                    // 普通行共用窗口底色；实色高亮足够标出选中行，省去软件阴影。
                    background: selected.then_some(Background::Color(SEL_BG)),
                    border: Border {
                        radius: RADIUS_ROW,
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            )
            .on_press(Message::Pick(idx))
            .on_right_press(Message::PickStay(idx));
            let row = if selected {
                row
            } else {
                // 滚动后同一个控件槽位可能显示另一条记录，按当前坐标反馈悬停。
                row.on_move(move |_| Message::Hover(idx))
            };

            (idx, Element::from(row))
        });

        // 等高占位保留完整滚动范围。普通 Column 会按类型更新控件状态，
        // 适合内容在占位与行之间替换，并会跳过视口外的绘制。
        let mut list = Column::with_capacity(window.len() * 2 + 2);
        if window.start != 0 {
            list = list.push(vspace(window.start as f32 * ROW_PITCH));
        }
        for (idx, row) in rows {
            list = list.push(row);
            if idx + 1 < clips.len() {
                list = list.push(rule::horizontal(ROW_SEPARATOR).style(rule_style));
            }
        }
        if window.end < clips.len() {
            list = list.push(vspace(history_height(clips.len() - window.end)));
        }
        scrollable(list)
            .id(self.list_id.clone())
            .height(Length::Fill)
            .width(Length::Fill)
            .on_scroll(Message::Scrolled)
            .style(scroll_style)
            .into()
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        let cur = self.cur_hash();
        let filtered = self.filtered();
        let query = self.query.trim();
        let shown = filtered.len().min(store::MENU_LIMIT);
        let selected = filtered.get(self.selected).copied();

        let placeholder = if self.search_mode {
            "搜索剪贴板历史"
        } else {
            "搜索剪贴板历史  (/)"
        };
        let search = text_input(placeholder, &self.query)
            .id(self.search_id.clone())
            .on_input(Message::Query)
            .on_paste(Message::Query)
            .size(15)
            .padding([8, 10])
            .style(prompt_style);
        let header = row![search, text(format!("{shown} 项")).size(12).color(MUTED)]
            .align_y(iced::Alignment::Center)
            .spacing(12)
            .padding([10, 12]);
        let mut col = iced::widget::column![header, rule::horizontal(1.0).style(rule_style)];
        if filtered.is_empty() {
            let label = if query.is_empty() {
                "暂无剪贴板历史"
            } else if self.search_in_flight {
                "正在搜索…"
            } else {
                "没有匹配的条目"
            };
            col = col.push(
                container(text(label).size(15).color(MUTED))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
            );
        } else {
            col = col.push(responsive(move |size| {
                self.history_list(&filtered[..shown], cur.as_deref(), size.height)
            }));
        }

        if self.confirm_delete {
            col = col.push(vspace(6.0)).push(
                container(text("再次按 Ctrl-X 删除固定条目，Esc 取消").size(12))
                    .width(Length::Fill)
                    .padding([6, 10])
                    .style(confirm_style),
            );
        }

        // 底部预览窗格：文本多行截断；图片条目直接渲染（iced image widget）。
        // 三种分支（文本/图片/缺失提示）外层一律定高 PREVIEW_HEIGHT——高度随
        // 内容变化会让上方 Fill 列表重排，切换条目时整窗闪烁跳动
        let Some(clip) = selected else {
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
                    let hint = if self.decode_failed.contains(&clip.id) || clip.image_path.is_none()
                    {
                        "无法预览这张图片"
                    } else {
                        "正在载入图片…"
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

        col = col.push(rule::horizontal(1.0).style(rule_style)).push(
            container(
                text("↑↓ 选择    Enter 复制    Ctrl-P 固定    Ctrl-X 删除")
                    .size(12)
                    .color(MUTED),
            )
            .padding([6, 12]),
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

    /// ▶ 标记取值：只读缓存，绝不做 IO（IO 由 refresh_cur 在消息路径完成）
    fn cur_hash(&self) -> Option<String> {
        self.cur_cache
            .borrow()
            .as_ref()
            .and_then(|(_, v)| v.clone())
    }

    /// 底部预览窗格内容：多行截断的唯一实现下沉在 core（任务 2.6 / C5），
    /// 此处只做 GUI 特有的处理——`↵` 在该字体下是 tofu，换成 `⏎`；
    /// 截断提示按 core 返回的 `truncated`（此前用字节长度比较，会漏判）
    fn preview_text(&self, clip: &store::Clip) -> String {
        let p =
            preview::preview_multiline(clip, preview::MULTILINE_GUI.0, preview::MULTILINE_GUI.1);
        let mut out = p.text.replace('↵', "⏎");
        if p.truncated {
            out.push('…');
        }
        out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_window_covers_partial_rows_without_laying_out_all_history() {
        let height = ROW_PITCH * 12.0;
        assert_eq!(history_window(300, 0.0, height), 0..15);
        assert_eq!(
            history_window(300, ROW_PITCH * 100.0 + 1.0, height),
            97..116
        );
    }

    #[test]
    fn history_window_clamps_after_search_or_resize() {
        let height = ROW_PITCH * 4.0;
        assert_eq!(history_window(20, 7000.0, height), 12..20);
        assert_eq!(history_window(3, 7000.0, height), 0..3);
        assert!(history_window(0, 7000.0, height).is_empty());
    }
}
