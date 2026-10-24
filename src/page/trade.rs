use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::*;

use super::{CardKind, WorkspacePage};
use crate::ui::root::RootView;

pub fn trade_page(
    page: &super::WorkspacePage,
    cx: &mut Context<crate::ui::root::RootView>,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_h_0()
        .child(page_toolbar(page, cx))
        .child(
            div()
                .id(("trade-cards", page.id))
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_3()
                .gap_2()
                .children(
                    page.cards
                        .iter()
                        .enumerate()
                        .map(|(index, kind)| card_panel(index, *kind, cx).into_any_element()),
                ),
        )
}

fn page_toolbar(page: &WorkspacePage, cx: &mut Context<RootView>) -> impl IntoElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_3()
        .h(px(30.))
        .bg(cx.theme().tab_bar)
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().child(page.symbol.clone()))
        .children([
            add_button(CardKind::Chart, "Add chart", cx),
            add_button(CardKind::Watchlist, "Add watchlist", cx),
            add_button(CardKind::Orders, "Add orders", cx),
            add_button(CardKind::Positions, "Add positions", cx),
        ])
}

fn add_button(kind: CardKind, label: &'static str, cx: &mut Context<RootView>) -> Button {
    Button::new(label)
        .label(label)
        .ghost()
        .small()
        .on_click(cx.listener(move |this, _, _, cx| this.add_card(kind, cx)))
}

fn card_panel(index: usize, kind: CardKind, cx: &mut Context<RootView>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .border_1()
        .border_color(cx.theme().border)
        .rounded_none()
        .child(card_header(index, kind, cx))
        .child(
            div()
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(placeholder(kind)),
        )
}

fn card_header(index: usize, kind: CardKind, cx: &mut Context<RootView>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .h(px(30.))
        .bg(cx.theme().tab_bar)
        .text_sm()
        .border_b_1()
        .border_color(cx.theme().border)
        .child(kind.title())
        .child(
            Button::new(("close-card", index))
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip("Remove card")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove_card(index, cx);
                })),
        )
}

fn placeholder(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Chart => "Chart placeholder — no live market data.",
        CardKind::Watchlist => "Watchlist placeholder — no live quotes.",
        CardKind::Orders => "Orders placeholder — no live order data.",
        CardKind::Positions => "Positions placeholder — no live position data.",
    }
}
