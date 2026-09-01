//! 消息处理与键路由（自 main.rs 拆出，纯代码搬移）。
//! App 字段与过滤/选中逻辑仍在 main.rs，跨模块方法以 pub(super) 互见。

use super::*;

impl App {
    pub(super) fn update(&mut self, message: Message) -> Task<Message> {
        if std::env::var_os("NIRI_CLIP_DEBUG").is_some() {
            eprintln!(
                "[dbg] msg={message:?} sel={} sid={:?}",
                self.selected, self.selected_id
            );
        }
        // 主体消息处理 + 选中图片的后台预解码派发（每条消息后检查一次：
        // 选中变了/解码完成都可能产生新的待解码目标，命中缓存则零开销）
        let task = self.handle_message(message);
        // ▶ 指针刷新只能在消息路径做：view 会被光标闪烁拉动重绘（~2Hz），
        // 渲染路径上的同步 fs IO 造成与光标同节奏的周期性闪烁（真机实锤）
        self.refresh_cur();
        let decode = self.ensure_image_decode();
        Task::batch([task, decode])
    }

    fn handle_message(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ImageReady { id, handle } => {
                self.decoding.remove(&id);
                match handle {
                    Some(h) => {
                        let mut cache = self.image_cache.borrow_mut();
                        cache.insert(0, (id, h));
                        cache.truncate(IMAGE_CACHE_CAP);
                    }
                    None => {
                        self.decode_failed.insert(id);
                    }
                }
                return Task::none();
            }
            Message::Tick => return Task::none(),
            Message::Query(q) => {
                // 同值回调必须忽略：搜索框持有焦点时，iced text_input 把
                // Ctrl+X 当"剪切"处理——空输入无编辑也发 on_input("")，且
                // 该消息先于订阅的 Key 到达。若在此复位选中，Ctrl-X 实际
                // 执行时 selected 已被归零 → 永远删掉顶部行（跳顶根因）
                if q == self.query {
                    return Task::none();
                }
                self.query = q;
                self.set_selection(0);
                self.confirm_delete = false;
                // 重新过滤后回到顶部；同时请求全库 FTS 候选（≥3 字符）
                return Task::batch([self.scroll_to_selected(), self.request_search()]);
            }
            Message::Key(key, modifiers) => return self.on_key(key, modifiers),
            Message::Hover(idx) => {
                // 仅在鼠标活跃时跟随：键盘滚动中忽略 on_enter（防闪烁）
                if self.mouse_follow && idx < self.visible_len() {
                    self.set_selection(idx);
                    // 选中已离开星标行：挂起的二段确认随之作废
                    self.confirm_delete = false;
                }
            }
            Message::MouseMove => {
                self.mouse_follow = true;
            }
            Message::Pick(idx) => {
                // 点击行 = 定位到该行并复制关闭（对齐 Enter）
                if idx < self.visible_len() {
                    self.set_selection(idx);
                    return Task::batch([
                        self.scroll_to_selected(),
                        self.update(Message::Copy { exit: true }),
                    ]);
                }
            }
            Message::PickStay(idx) => {
                // 右键行 = 定位到该行并连续复制（对齐 Ctrl-Y，不退出）
                if idx < self.visible_len() {
                    self.set_selection(idx);
                    return Task::batch([
                        self.scroll_to_selected(),
                        self.update(Message::Copy { exit: false }),
                    ]);
                }
            }
            Message::Scrolled(vp) => {
                // 真实视口高度回填：resize / 预览高度变化后滚动跟随仍居中
                let h = vp.bounds().height;
                if h > 1.0 {
                    self.viewport_half = (h / 2.0).clamp(120.0, 400.0);
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
                        // panic 按失败处理：触发失败通知而非静默吞掉
                        Message::CopyFinished { exit, ok: false },
                    );
                }
            }
            Message::CopyFinished { exit, ok } => {
                if !ok {
                    // 通知开关：失败反馈（窗口即将退出，stderr 看不见）
                    if self.notify_enabled {
                        niri_clip_core::notify::send("复制失败");
                    } else {
                        eprintln!("[niri-clip gui] copy failed");
                    }
                }
                if exit {
                    // 后台复制已完成，wl-copy 守护进程持有数据，退出安全
                    std::process::exit(0);
                }
                // Ctrl-Y 连续复制：重拉列表，▶ 已随 copy 刷新到刚复制的条目。
                // selected_id 先记到旧列表第 1 行（复制目标），重载后由
                // relocate_selected 跟随——即使它已因 ▶ 置顶排序移动
                self.query.clear();
                self.set_selection(0);
                return Task::batch([
                    run_bg(
                        move || store::list(Self::load_limit()).ok(),
                        Message::ListReloaded,
                        Message::ListReloaded(None),
                    ),
                    self.scroll_to_selected(),
                ]);
            }
            Message::ListReloaded(Some(clips)) => {
                self.clips = clips;
                self.clips_gen += 1;
                // Ctrl-P 等操作会重排行序：按 id 跟随选中，而非保留索引
                self.relocate_selected();
                return Task::batch([self.scroll_to_selected(), self.request_search()]);
            }
            Message::DeleteReloaded(Some(clips)) => {
                self.clips = clips;
                self.clips_gen += 1;
                // 行上移会让静止指针下换行，抑制悬停跟随直到真实移动
                self.mouse_follow = false;
                self.confirm_delete = false;
                // 被删条目的 id 已不在 → 保留索引由下一行顶上，末行回退；
                // 若 delete 失败（列表未变）则按 id 精确回到原选中
                self.relocate_selected();
                // 不发 scroll_to：保持滚动位置，选中由行上移自然锚定
                return self.request_search();
            }
            Message::DeleteReloaded(None) => {}
            Message::ListReloaded(None) => {}
            Message::SearchDone { query, gen, hits } => {
                // 新鲜度检查：期间已继续输入或列表重载 → 丢弃过期结果
                if query == self.query && gen == self.clips_gen {
                    self.search_hits = hits;
                    self.search_hits_query = query;
                    self.search_hits_gen = gen;
                }
            }
            Message::Refocus => {
                // 重新聚焦重拉：daemon 在窗口失焦期间捕获的新内容补进来。
                // 不滚动、按 id 重定位——打开期间的浏览位置不受影响
                return run_bg(
                    move || store::list(Self::load_limit()).ok(),
                    Message::DeleteReloaded,
                    Message::DeleteReloaded(None),
                );
            }
        }
        Task::none()
    }

    pub(super) fn on_key(
        &mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) -> Task<Message> {
        // 仅导航/动作键；普通字符、Backspace、Space、IME 提交由持焦点的
        // text_input 自行处理（winit 原生 IME，中文可用）
        match key {
            keyboard::Key::Named(keyboard::key::Named::ArrowUp) => return self.move_selection(-1),
            keyboard::Key::Named(keyboard::key::Named::ArrowDown) => return self.move_selection(1),
            keyboard::Key::Character(c)
                if c == "/" && !modifiers.control() && !self.search_mode =>
            {
                // fzf 语义：/ 进入搜索（Ctrl-F 同效）。输入框获得焦点后
                // 光标才开始闪烁；未进入时字符不进入搜索，导航/快选不受影响
                self.search_mode = true;
                return iced::widget::operation::focus(self.search_id.clone());
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "f" && !self.search_mode => {
                self.search_mode = true;
                return iced::widget::operation::focus(self.search_id.clone());
            }
            keyboard::Key::Named(keyboard::key::Named::Escape) => {
                // fzf 语义：有输入/确认时先取消，空查询才退出。
                // 退出搜索时轮换输入框 Id：旧 widget 状态连同焦点一起丢弃，
                // 光标熄灭 → 窗口回到零重绘待机（周期性闪烁的最后来源）
                if self.search_mode || !self.query.is_empty() {
                    self.search_mode = false;
                    self.query.clear();
                    self.confirm_delete = false;
                    self.set_selection(0);
                    self.search_id = iced::widget::Id::unique();
                    return self.scroll_to_selected();
                }
                if self.confirm_delete {
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
                let notify = self.notify_enabled;
                return run_bg(
                    move || {
                        let ok = store::toggle_pin(id).is_ok();
                        (ok, store::list(App::load_limit()).ok())
                    },
                    move |(ok, clips)| {
                        if !ok {
                            if notify {
                                niri_clip_core::notify::send("固定/取消固定失败");
                            } else {
                                eprintln!("[niri-clip gui] pin failed");
                            }
                        }
                        Message::ListReloaded(clips)
                    },
                    Message::ListReloaded(None),
                );
            }
            keyboard::Key::Character(c) if modifiers.control() && c == "x" => {
                return self.delete_selected();
            }
            keyboard::Key::Character(c)
                if modifiers.alt()
                    && c.len() == 1
                    && (c.as_str() >= "1" && c.as_str() <= "9" || c.as_str() == "0") =>
            {
                // Alt+1-9,0 快选：搜索态下裸数字被持焦点输入框接管，
                // Alt 组合键让快选在搜索态继续可用（0 = 第 10 行）；
                // 非搜索态同样生效，与裸数字等价冗余
                let n: usize = if c.as_str() == "0" {
                    10
                } else {
                    c.parse().unwrap_or(0)
                };
                if n >= 1 && n <= self.visible_len() {
                    self.set_selection(n - 1);
                    return self.update(Message::Copy { exit: true });
                }
            }
            keyboard::Key::Character(c)
                if self.query.is_empty()
                    && !modifiers.control()
                    && !modifiers.alt()
                    && c.len() == 1
                    && (c.as_str() >= "1" && c.as_str() <= "9" || c.as_str() == "0") =>
            {
                // 空查询时 1-9,0 快选（0=第 10 行）：定位到过滤列表第 n 行并
                // 复制关闭；有输入时数字回落为查询字符（text_input 自行处理），
                // 搜索态快选走上方 Alt 组合键
                let n: usize = if c.as_str() == "0" {
                    10
                } else {
                    c.parse().unwrap_or(0)
                };
                if n >= 1 && n <= self.visible_len() {
                    self.set_selection(n - 1);
                    return self.update(Message::Copy { exit: true });
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn move_selection(&mut self, delta: i32) -> Task<Message> {
        // 键盘导航接管选中：暂停悬停跟随（列表滚过静止指针会触发
        // 一串 on_enter，把选中态抢回去——闪烁根因）
        self.mouse_follow = false;
        let n = self.visible_len();
        if n > 0 {
            let next = self.selected as i64 + delta as i64;
            let next = next.clamp(0, n as i64 - 1) as usize;
            if next != self.selected {
                // 选中已离开星标行：挂起的二段确认随之作废
                self.confirm_delete = false;
                self.set_selection(next);
            }
            // 键盘导航滚动跟随：把选中行滚进可视区（视口半高估算）
            return self.scroll_to_selected();
        }
        Task::none()
    }

    /// 把选中行滚动到可视区中部（行高定长 ROW_PITCH，偏移可精确计算；
    /// 视口半高用 on_scroll 回填的实测值，resize 自适应）
    pub(super) fn scroll_to_selected(&self) -> Task<Message> {
        let y = ((self.selected as f32) * ROW_PITCH - self.viewport_half).max(0.0);
        operation::scroll_to(
            self.list_id.clone(),
            operation::AbsoluteOffset { x: 0.0, y },
        )
    }

    pub(super) fn delete_selected(&mut self) -> Task<Message> {
        let Some(clip) = self.filtered().get(self.selected).cloned() else {
            return Task::none();
        };
        if std::env::var_os("NIRI_CLIP_DEBUG").is_some() {
            eprintln!(
                "[dbg] delete_selected sel={} sid={:?} -> id={} pinned={}",
                self.selected, self.selected_id, clip.id, clip.pinned
            );
        }
        // 星标条目二段确认：第一次 Ctrl-X 仅挂起确认，再按才执行
        if clip.pinned && !self.confirm_delete {
            self.confirm_delete = true;
            return Task::none();
        }
        let id = clip.id;
        // 后台删除 + 重拉列表，UI 线程零阻塞（sqlite 写锁最长 busy_timeout 5s）
        self.confirm_delete = false;
        // 保留查询与选中索引（fzf --track 删除跟随语义）：ListReloaded 后
        // 同索引 = 下一行顶上，删可见区末尾则 clamp 到上一行。清查询会让
        // 过滤列表突变为全库，选中看起来"弹回顶部"
        let notify = self.notify_enabled;
        run_bg(
            move || {
                let ok = store::delete(id).is_ok();
                (ok, store::list(App::load_limit()).ok())
            },
            move |(ok, clips)| {
                if !ok {
                    if notify {
                        niri_clip_core::notify::send("删除失败");
                    } else {
                        eprintln!("[niri-clip gui] delete failed");
                    }
                }
                Message::DeleteReloaded(clips)
            },
            Message::DeleteReloaded(None),
        )
    }
    /// ▶ 指针缓存刷新：只在 update（真实消息）路径执行，绝不进 view——
    /// view 会被搜索框光标闪烁周期性拉动重绘（~2Hz），渲染路径上的同步
    /// fs IO（state/current 文件读）会造成与光标同节奏的周期性掉帧/背景
    /// 闪烁。view 只消费缓存（见 cur_hash）
    pub(super) fn refresh_cur(&mut self) {
        const TTL: Duration = Duration::from_millis(500);
        let expired = self
            .cur_cache
            .borrow()
            .as_ref()
            .map(|(at, _)| at.elapsed() >= TTL)
            .unwrap_or(true);
        if !expired {
            return;
        }
        let v = store::current_hash();
        *self.cur_cache.borrow_mut() = Some((Instant::now(), v));
    }
}
