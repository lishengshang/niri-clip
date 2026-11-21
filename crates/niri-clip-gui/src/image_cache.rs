//! 原生窗口的图片预览。原图用于复制，窗口缓存缩略图。

use super::*;

// 默认窗口约 500 逻辑像素宽，按双倍显示密度保留预览细节。
const PREVIEW_PIXEL_WIDTH: u32 = 1024;
const PREVIEW_PIXEL_HEIGHT: u32 = PREVIEW_HEIGHT as u32 * 2;

fn preview_pixels(img: ::image::DynamicImage) -> ::image::RgbaImage {
    if img.width() > PREVIEW_PIXEL_WIDTH || img.height() > PREVIEW_PIXEL_HEIGHT {
        img.thumbnail(PREVIEW_PIXEL_WIDTH, PREVIEW_PIXEL_HEIGHT)
            .into_rgba8()
    } else {
        img.into_rgba8()
    }
}

fn load_preview(path: &str) -> Option<image::Handle> {
    // store 按 id 保存为 .bin，格式由文件头识别。
    let img = ::image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let rgba = preview_pixels(img);
    Some(image::Handle::from_rgba(
        rgba.width(),
        rgba.height(),
        rgba.into_raw(),
    ))
}

impl App {
    pub(super) fn ensure_image_decode(&mut self) -> Task<Message> {
        if self.decoding || !self.enable_preview || !self.image_preview_enabled {
            return Task::none();
        }
        let filtered = self.filtered();
        let Some(clip) = filtered.get(self.selected) else {
            return Task::none();
        };
        let id = clip.id;
        if !clip.mime.starts_with("image/") || self.decode_failed.contains(&id) {
            return Task::none();
        }
        if self.image_cache.borrow().iter().any(|(cid, _)| *cid == id) {
            return Task::none();
        }
        let Some(path) = clip.image_path.clone() else {
            return Task::none();
        };
        self.decoding = true;
        run_bg(
            move || load_preview(&path),
            move |handle| Message::ImageReady { id, handle },
            Message::ImageReady { id, handle: None },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnails_fit_the_pane_and_preserve_small_images() {
        for (width, height, expected) in [
            (2048, 200, (1024, 100)),
            (120, 880, (60, 440)),
            (17, 11, (17, 11)),
        ] {
            let img = ::image::DynamicImage::new_rgba8(width, height);
            assert_eq!(preview_pixels(img).dimensions(), expected);
        }
    }
}
