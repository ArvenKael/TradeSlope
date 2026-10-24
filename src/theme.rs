use gpui_kit::component::theme::{ActiveTheme, Theme};
use gpui_kit::{App, px, rgb};

pub use gpui_kit::component::theme::ThemeMode;

pub fn toggle_theme(cx: &mut App) {
    let mode = if cx.theme().mode.is_dark() {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    set_theme(mode, cx);
}

pub fn set_theme(mode: ThemeMode, cx: &mut App) {
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(13.);
        theme.radius = px(2.);
        theme.radius_lg = px(2.);
        theme.shadow = false;
        let (background, panel, border, foreground, muted, selected) = if mode.is_dark() {
            (0x1e1e22, 0x27272b, 0x38383d, 0xd4d4d8, 0x929298, 0x34343b)
        } else {
            (0xfafafa, 0xf0f0f2, 0xd7d7dc, 0x303036, 0x73737b, 0xe1e1e7)
        };
        theme.colors.background = rgb(background).into();
        theme.colors.foreground = rgb(foreground).into();
        theme.colors.muted_foreground = rgb(muted).into();
        theme.colors.border = rgb(border).into();
        theme.colors.sidebar = rgb(panel).into();
        theme.colors.sidebar_foreground = rgb(foreground).into();
        theme.colors.tab_bar = rgb(panel).into();
        theme.colors.title_bar = rgb(if mode.is_dark() { 0x353539 } else { 0xe5e5e8 }).into();
        theme.colors.title_bar_border = rgb(border).into();
        theme.colors.tab_active = rgb(background).into();
        theme.colors.tab_active_foreground = rgb(foreground).into();
        theme.colors.accent = rgb(selected).into();
    });
    cx.refresh_windows();
}
