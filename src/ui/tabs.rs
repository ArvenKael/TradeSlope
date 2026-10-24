use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::*;

use super::root::RootView;
use crate::page::{PageId, WorkspacePage};

pub struct TabManager {
    pages: Vec<PageId>,
    selected: usize,
}

impl TabManager {
    pub fn new() -> Self {
        Self {
            pages: vec![0],
            selected: 0,
        }
    }

    pub fn active_page(&self) -> Option<PageId> {
        self.pages.get(self.selected).copied()
    }

    pub fn close(&mut self, index: usize) {
        if index >= self.pages.len() {
            return;
        }
        self.pages.remove(index);
        if index < self.selected {
            self.selected -= 1;
        } else {
            self.selected = self.selected.min(self.pages.len().saturating_sub(1));
        }
    }

    pub fn open(&mut self, page: PageId) {
        self.selected = self
            .pages
            .iter()
            .position(|existing| *existing == page)
            .unwrap_or_else(|| {
                self.pages.push(page);
                self.pages.len() - 1
            });
    }

    pub fn select(&mut self, index: usize) {
        if index < self.pages.len() {
            self.selected = index;
        }
    }
}

pub fn tab_bar(
    tabs: &TabManager,
    pages: &[WorkspacePage],
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .id("page-tabs")
        .flex()
        .h(px(32.))
        .overflow_x_scroll()
        .children(tabs.pages.iter().enumerate().filter_map(|(index, id)| {
            pages
                .iter()
                .find(|page| page.id == *id)
                .map(|page| render_tab(index, page, index == tabs.selected, cx).into_any_element())
        }))
}

fn render_tab(
    index: usize,
    page: &WorkspacePage,
    selected: bool,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    div()
        .id(("page-tab", page.id))
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .h_full()
        .flex_shrink_0()
        .border_r_1()
        .border_color(cx.theme().border)
        .bg(if selected {
            cx.theme().background
        } else {
            cx.theme().tab_bar
        })
        .text_color(if selected {
            cx.theme().foreground
        } else {
            cx.theme().muted_foreground
        })
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, window, cx| this.select_tab(index, window, cx)))
        .child(page.name.clone())
        .child(
            Button::new(("close-tab", index))
                .icon(IconName::Close)
                .ghost()
                .xsmall()
                .tooltip("Close tab")
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_tab(index, window, cx);
                })),
        )
}
