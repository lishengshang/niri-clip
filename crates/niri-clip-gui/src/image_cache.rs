//! 图片后台预解码（自 main.rs 拆出，纯代码搬移）：后台线程
//! load_from_memory → RGBA Handle，渲染器零解码开销（详见方法注释）。

use super::*;

impl App {
    /// 选中项为图片且未解码时，派发后台解码任务。
    /// 曾在 view 里同步 fs::read + 让 tiny-skia 在 layout 阶段同步解码：
    /// 一张未缓存截图冻结 UI 线程几十至上百毫秒（期间屏幕停在旧帧，
    /// 解完后新帧突然出现）——快速导航跨多个图片条目时闪烁/卡顿明显。
    /// 现改为后台线程解码成 RGBA（Handle::from_rgba），渲染器零解码开销
    pub(super) fn ensure_image_decode(&mut self) -> Task<Message> {
        if !self.enable_preview || !self.image_preview_enabled {
            return Task::none();
        }
        let filtered = self.filtered();
        let Some(clip) = filtered.get(self.selected) else {
            return Task::none();
        };
        if !clip.mime.starts_with("image/") || clip.image_path.is_none() {
            return Task::none();
        }
        let id = clip.id;
        if self.decoding.contains(&id) || self.decode_failed.contains(&id) {
            return Task::none();
        }
        if self.image_cache.borrow().iter().any(|(cid, _)| *cid == id) {
            return Task::none();
        }
        let Some(path) = clip.image_path.clone() else {
            return Task::none();
        };
        self.decoding.insert(id);
        run_bg(
            move || {
                let handle = std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| ::image::load_from_memory(&bytes).ok())
                    .map(|img| {
                        let rgba = img.to_rgba8();
                        image::Handle::from_rgba(img.width(), img.height(), rgba.into_raw())
                    });
                Message::ImageReady { id, handle }
            },
            |m| m,
            Message::ImageReady { id, handle: None },
        )
    }
}
