use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::*;

use super::root::RootView;
use crate::page::{PageId, WorkspacePage};

pub fn sidebar(
    pages: &[WorkspacePage],
    active: Option<PageId>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .w(px(200.))
        .h_full()
        .flex_shrink_0()
        .bg(cx.theme().sidebar)
        .border_r_1()
        .border_color(cx.theme().border)
        .child(sidebar_header(cx))
        .child(
            div()
                .id("page-list")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .children(
                    pages
                        .iter()
                        .map(|page| page_row(page, active, cx).into_any_element()),
                ),
        )
        .child(
            div()
                .h(px(30.))
                .border_t_1()
                .border_color(cx.theme().border)
                .child(theme_button(cx)),
        )
}

fn page_row(
    page: &WorkspacePage,
    active: Option<PageId>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let id = page.id;
    let color = if active == Some(id) {
        cx.theme().accent
    } else {
        cx.theme().sidebar
    };
    div()
        .id(("sidebar-page", id))
        .flex()
        .items_center()
        .gap_2()
        .h(px(28.))
        .px_3()
        .bg(color)
        .cursor_pointer()
        .hover(|style| style.bg(cx.theme().accent))
        .child(
            Icon::new(IconName::FileText)
                .text_color(cx.theme().muted_foreground)
                .small(),
        )
        .child(page.name.clone())
        .on_click(cx.listener(move |this, _, window, cx| this.open_page(id, window, cx)))
}

fn sidebar_header(cx: &mut Context<RootView>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(32.))
        .px_3()
        .text_color(cx.theme().muted_foreground)
        .child("Pages")
        .child(
            Button::new("new-page")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New page")
                .on_click(cx.listener(|this, _, window, cx| this.new_page(window, cx))),
        )
}

fn theme_button(cx: &mut Context<RootView>) -> impl IntoElement {
    let dark = cx.theme().mode.is_dark();
    Button::new("toggle-theme")
        .ghost()
        .xsmall()
        .icon(if dark { IconName::Sun } else { IconName::Moon })
        .label(if dark { "Light theme" } else { "Dark theme" })
        .on_click(|_, _, cx| crate::theme::toggle_theme(cx))
}

pub fn sidebar_toggle(visible: bool, cx: &mut Context<RootView>) -> impl IntoElement {
    Button::new("toggle-sidebar")
        .ghost()
        .xsmall()
        .icon(Icon::default().path(if visible {
            "icons/panel-left.svg"
        } else {
            "icons/zed-sidebar-closed.svg"
        }))
        .tooltip("Toggle page sidebar")
        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
}
