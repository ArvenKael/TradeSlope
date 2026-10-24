use super::sidebar::sidebar;
use super::tabs::{TabManager, tab_bar};
use super::title_bar::title_bar;
use crate::page::{CardKind, PageId, WorkspacePage, trade::trade_page, welcome::welcome_page};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::*;

pub struct RootView {
    tabs: TabManager,
    pages: Vec<WorkspacePage>,
    sidebar_visible: bool,
    editing_page: bool,
    page_name: Entity<InputState>,
    symbol: Entity<InputState>,
    next_page_id: PageId,
}

impl RootView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            tabs: TabManager::new(),
            pages: vec![WorkspacePage {
                id: 0,
                name: "Trading".into(),
                symbol: "BTC".into(),
                cards: vec![CardKind::Chart],
            }],
            sidebar_visible: true,
            editing_page: false,
            page_name: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Page name")
                    .default_value("Trading")
            }),
            symbol: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Symbol")
                    .default_value("BTC")
            }),
            next_page_id: 1,
        }
    }

    pub(super) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
    }

    pub(super) fn new_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_page_id;
        self.next_page_id += 1;
        self.pages.push(WorkspacePage {
            id,
            name: format!("Untitled {}", id),
            symbol: "BTC".into(),
            cards: vec![],
        });
        self.editing_page = true;
        self.open_page(id, window, cx);
    }

    pub(super) fn open_page(&mut self, id: PageId, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.open(id);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    pub(super) fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.select(index);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    pub(super) fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs.close(index);
        self.sync_page_fields(window, cx);
        cx.notify();
    }

    fn active_page(&self) -> Option<&WorkspacePage> {
        let id = self.tabs.active_page()?;
        self.pages.iter().find(|page| page.id == id)
    }

    fn sync_page_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (name, symbol) = self
            .active_page()
            .map(|page| (page.name.clone(), page.symbol.clone()))
            .unwrap_or_default();
        self.page_name
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.symbol
            .update(cx, |state, cx| state.set_value(symbol, window, cx));
    }

    fn apply_page_settings(&mut self, cx: &mut Context<Self>) {
        let name = self.page_name.read(cx).value().trim().to_string();
        let symbol = self.symbol.read(cx).value().trim().to_uppercase();
        if name.is_empty() || symbol.is_empty() {
            return;
        }
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.name = name;
            page.symbol = symbol;
        }
        self.editing_page = false;
        cx.notify();
    }

    pub(crate) fn add_card(&mut self, kind: CardKind, cx: &mut Context<Self>) {
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            page.cards.push(kind);
        }
        cx.notify();
    }

    pub(crate) fn remove_card(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.tabs.active_page();
        if let Some(page) = self.pages.iter_mut().find(|page| Some(page.id) == id) {
            if index < page.cards.len() {
                page.cards.remove(index);
            }
        }
        cx.notify();
    }

    fn render_page_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .child(div().w(px(180.)).child(Input::new(&self.page_name).small()))
            .child(div().w(px(100.)).child(Input::new(&self.symbol).small()))
            .child(
                Button::new("apply-page")
                    .ghost()
                    .small()
                    .label("Apply")
                    .on_click(cx.listener(|this, _, _, cx| this.apply_page_settings(cx))),
            )
    }

    fn render_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .overflow_hidden();
        if let Some(page) = self.active_page() {
            if self.editing_page {
                content = content.child(self.render_page_settings(cx));
            }
            content = content.child(trade_page(page, cx));
        } else {
            content = content.child(welcome_page());
        }
        content
    }

    fn render_workspace(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(32.))
                    .flex_shrink_0()
                    .bg(cx.theme().tab_bar)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(tab_bar(&self.tabs, &self.pages, cx)),
                    )
                    .child(
                        Button::new("edit-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Settings2)
                            .tooltip("Page name and symbol")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing_page = !this.editing_page;
                                cx.notify();
                            })),
                    ),
            )
            .child(self.render_page(cx))
    }
}

impl Render for RootView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut layout = div().flex().flex_1().min_h_0();
        if self.sidebar_visible {
            layout = layout.child(sidebar(&self.pages, self.tabs.active_page(), cx));
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(cx.theme().background)
            .text_size(px(13.))
            .text_color(cx.theme().foreground)
            .child(title_bar(
                self.active_page().map(|page| page.name.as_str()),
                self.sidebar_visible,
                cx,
            ))
            .child(layout.child(self.render_workspace(cx)))
    }
}
