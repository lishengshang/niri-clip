//! 配色常量与全部控件样式函数。
//!
//! 视觉语言对齐 fzf 默认深色风格（与 layer-shell 旧版一脉相承）：
//! 深底 + accent 蓝键位 + hl 红命中高亮 + 圆角面板 + 垂直偏移阴影。

use iced::widget::{container, rule, scrollable, text_input};
use iced::{border, Background, Border, Color, Shadow, Vector};

// 配色对齐 fzf 默认深色风格（与 layer-shell 旧版一致的视觉语言）
pub const BG: Color = Color {
    r: 0.086,
    g: 0.086,
    b: 0.110,
    a: 1.0,
};
/// 面板色：搜索框 / 预览窗格底色（比 BG 微亮一档）
pub const PANEL: Color = Color {
    r: 0.110,
    g: 0.110,
    b: 0.140,
    a: 1.0,
};
pub const BORDER: Color = Color {
    r: 0.170,
    g: 0.170,
    b: 0.220,
    a: 1.0,
};
pub const ROW_FG: Color = Color {
    r: 0.88,
    g: 0.88,
    b: 0.91,
    a: 1.0,
};
pub const ROW_FG_SELECTED: Color = Color {
    r: 0.96,
    g: 0.96,
    b: 0.98,
    a: 1.0,
};
/// 次要文本：header 提示行 / 占位符 / 预览
pub const MUTED: Color = Color {
    r: 0.545,
    g: 0.570,
    b: 0.650,
    a: 1.0,
};
/// 命中字符高亮（fzf 默认 hl：深红）
pub const HL: Color = Color {
    r: 0.949,
    g: 0.467,
    b: 0.478,
    a: 1.0,
};
pub const ACCENT: Color = Color {
    r: 0.48,
    g: 0.64,
    b: 0.97,
    a: 1.0,
};
pub const SEL_BG: Color = Color {
    r: 0.16,
    g: 0.28,
    b: 0.50,
    a: 1.0,
};
pub const SCROLLBAR: Color = Color {
    r: 0.230,
    g: 0.230,
    b: 0.290,
    a: 1.0,
};

pub const RADIUS_ROW: border::Radius = border::Radius {
    top_left: 4.0,
    top_right: 4.0,
    bottom_right: 4.0,
    bottom_left: 4.0,
};
pub const RADIUS_PANEL: border::Radius = border::Radius {
    top_left: 6.0,
    top_right: 6.0,
    bottom_right: 6.0,
    bottom_left: 6.0,
};

/// 面板阴影（立体感）：黑色低透明 + 垂直偏移
pub const SHADOW_PANEL: Shadow = Shadow {
    color: Color {
        a: 0.35,
        r: 0.0,
        g: 0.0,
        b: 0.0,
    },
    offset: Vector { x: 0.0, y: 2.0 },
    blur_radius: 10.0,
};
/// 选中行微阴影
pub const SHADOW_ROW: Shadow = Shadow {
    color: Color {
        a: 0.30,
        r: 0.0,
        g: 0.0,
        b: 0.0,
    },
    offset: Vector { x: 0.0, y: 1.0 },
    blur_radius: 5.0,
};

pub fn prompt_style(_theme: &iced::Theme, _status: text_input::Status) -> text_input::Style {
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

pub fn rule_style(_theme: &iced::Theme) -> rule::Style {
    rule::Style {
        color: BORDER,
        radius: border::Radius::default(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

pub fn scroll_style(_theme: &iced::Theme, status: scrollable::Status) -> scrollable::Style {
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

pub fn preview_style(_theme: &iced::Theme) -> container::Style {
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

pub fn confirm_style(_theme: &iced::Theme) -> container::Style {
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
