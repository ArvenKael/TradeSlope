use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, Sizable, TitleBar};
use gpui_kit::*;

use super::root::RootView;
use super::sidebar::sidebar_toggle;

pub fn title_bar(
    page_name: Option<&str>,
    sidebar_visible: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    TitleBar::new()
        .bg(cx.theme().title_bar)
        .border_color(cx.theme().border)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .size_full()
                .pr_2()
                .child(sidebar_toggle(sidebar_visible, cx))
                .child(div().text_size(px(12.)).child("Trade Slope"))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.))
                        .text_color(cx.theme().muted_foreground)
                        .child(page_name.unwrap_or("Workspace").to_string()),
                )
                .child(
                    Button::new("title-new-page")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("New page")
                        .on_click(cx.listener(|this, _, window, cx| this.new_page(window, cx))),
                ),
        )
}
