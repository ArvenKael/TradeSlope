use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::*;

use super::workspace::{MIN_CARD_HEIGHT, MIN_CARD_WIDTH, Pane, PaneId, SplitDirection};
use super::{CardKind, WorkspacePage};
use crate::extensions::Extensions;
use crate::ui::native_cards::NativeCards;
use crate::ui::root::RootView;
use crate::ui::web_cards::WebCards;

pub struct CardViews<'a> {
    pub web: &'a WebCards,
    pub native: &'a NativeCards,
    pub extensions: &'a Extensions,
    pub backend: &'a tradeslope_sdk::BackendClient,
    pub picker: Option<((super::PageId, PaneId), AnyView)>,
    pub advanced: &'a crate::ui::connections::Connections,
}

#[derive(Clone)]
struct CardDrag {
    page: super::PageId,
    card: PaneId,
    title: SharedString,
}

impl Render for CardDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(MIN_CARD_WIDTH))
            .h(px(MIN_CARD_HEIGHT))
            .flex()
            .flex_col()
            .bg(cx.theme().tab_bar)
            .border_1()
            .border_color(cx.theme().muted_foreground)
            .opacity(0.85)
            .child(div().h(px(28.)).px_3().child(self.title.clone()))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().background)
                    .text_sm()
                    .child("Moving card"),
            )
    }
}

pub fn workspace_page(
    page: &WorkspacePage,
    views: &CardViews<'_>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let (minimum_width, minimum_height) = page
        .workspace
        .root
        .as_ref()
        .map(Pane::minimum_size)
        .unwrap_or((MIN_CARD_WIDTH, MIN_CARD_HEIGHT));
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_h_0()
        .child(page_toolbar(cx))
        .child(
            div()
                .id(("workspace-scroll", page.id))
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_x_scroll()
                .overflow_y_scroll()
                .p_1()
                .child(
                    div()
                        .size_full()
                        .min_w(px(minimum_width))
                        .min_h(px(minimum_height))
                        .child(match &page.workspace.root {
                            Some(root) => render_pane(root, page, views, cx),
                            None => div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_color(cx.theme().muted_foreground)
                                .child("No cards. Use + above to add a view.")
                                .into_any_element(),
                        }),
                ),
        )
}

fn page_toolbar(cx: &mut Context<RootView>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .px_3()
        .h(px(30.))
        .flex_shrink_0()
        .bg(cx.theme().tab_bar)
        .border_b_1()
        .border_color(cx.theme().border)
        .child(div().text_sm().child("Choose a source inside each card"))
        .child(
            Button::new("add-card")
                .icon(IconName::Plus)
                .ghost()
                .xsmall()
                .tooltip("Add card")
                .on_click(cx.listener(|this, _, _, cx| this.add_card(cx))),
        )
}

fn render_pane(
    pane: &Pane,
    page: &WorkspacePage,
    views: &CardViews<'_>,
    cx: &mut Context<RootView>,
) -> AnyElement {
    match pane {
        Pane::Card { id, kind } => {
            card_panel(*id, kind.as_ref(), page, views, cx).into_any_element()
        }
        Pane::Split {
            id,
            direction,
            first,
            second,
        } => {
            let key = SharedString::from(format!("page-{}-split-{id}", page.id));
            let mut group = match direction {
                SplitDirection::Right => h_resizable(key),
                SplitDirection::Below => v_resizable(key),
            };
            if let Some(state) = page.workspace.sizes.get(id) {
                group = group.with_state(state);
            }
            let minimum = |pane: &Pane| {
                let (width, height) = pane.minimum_size();
                px(match direction {
                    SplitDirection::Right => width,
                    SplitDirection::Below => height,
                })
            };
            group
                .child(
                    resizable_panel()
                        .size_range(minimum(first)..Pixels::MAX)
                        .child(render_pane(first, page, views, cx)),
                )
                .child(
                    resizable_panel()
                        .size_range(minimum(second)..Pixels::MAX)
                        .child(render_pane(second, page, views, cx)),
                )
                .into_any_element()
        }
    }
}

fn view_picker(
    id: PaneId,
    kind: Option<&CardKind>,
    extensions: &Extensions,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let view = cx.entity().downgrade();
    let label = kind
        .map(|kind| extensions.title(kind))
        .unwrap_or_else(|| "Starter Pane".into());
    let selected = kind.cloned();
    let choices: Vec<_> = std::iter::once((None, "Starter Pane".to_string()))
        .chain(
            extensions
                .choices()
                .into_iter()
                .map(|(kind, title)| (Some(kind), title)),
        )
        .collect();
    Button::new(("choose-view", id))
        .label(label)
        .icon(IconName::ChevronDown)
        .ghost()
        .xsmall()
        .w(px(165.))
        .dropdown_menu(move |mut menu, _, _| {
            for (choice, title) in &choices {
                let view = view.clone();
                let choice = choice.clone();
                menu = menu.item(
                    PopupMenuItem::new(title.clone())
                        .checked(choice == selected)
                        .on_click(move |_, _, cx| {
                            let _ = view
                                .update(cx, |this, cx| this.choose_card(id, choice.clone(), cx));
                        }),
                );
            }
            menu
        })
}

fn card_menu(
    id: PaneId,
    kind: Option<&CardKind>,
    extensions: &Extensions,
    bindings: Option<&tradeslope_sdk::Bindings>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let view = cx.entity().downgrade();
    let connected: Vec<_> = kind
        .and_then(|kind| extensions.manifest(kind))
        .map(|manifest| {
            manifest
                .inputs
                .into_iter()
                .filter(|port| bindings.is_some_and(|bindings| bindings.contains_key(&port.id)))
                .map(|port| (port.id, port.title))
                .collect()
        })
        .unwrap_or_default();
    let selected = kind.cloned();
    let choices: Vec<_> = std::iter::once((None, "Starter Pane".to_string()))
        .chain(
            extensions
                .choices()
                .into_iter()
                .map(|(kind, title)| (Some(kind), title)),
        )
        .collect();
    Button::new(("card-menu", id))
        .label("⋯")
        .ghost()
        .xsmall()
        .tooltip("Card settings")
        .dropdown_menu(move |menu, window, cx| {
            let inputs_view = view.clone();
            let remove_view = view.clone();
            let choices_view = view.clone();
            let advanced_view = view.clone();
            let disconnect_view = view.clone();
            let connected = connected.clone();
            let selected = selected.clone();
            let choices = choices.clone();
            menu.item(
                PopupMenuItem::new("Manage sources").on_click(move |_, window, cx| {
                    let _ = inputs_view
                        .update(cx, |this, cx| this.choose_card_source(id, None, window, cx));
                }),
            )
            .submenu("Disconnect source", window, cx, move |mut menu, _, _| {
                for (port, title) in &connected {
                    let view = disconnect_view.clone();
                    let port = port.clone();
                    menu =
                        menu.item(PopupMenuItem::new(title.clone()).on_click(move |_, _, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.disconnect_card_source(id, port.clone(), cx)
                            });
                        }));
                }
                menu
            })
            .submenu("Change view", window, cx, move |mut menu, _, _| {
                for (choice, title) in &choices {
                    let view = choices_view.clone();
                    let choice = choice.clone();
                    menu = menu.item(
                        PopupMenuItem::new(title.clone())
                            .checked(choice == selected)
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.choose_card(id, choice.clone(), cx)
                                });
                            }),
                    );
                }
                menu
            })
            .separator()
            .item(PopupMenuItem::new("Advanced mapping (developer)").on_click(
                move |_, window, cx| {
                    let _ = advanced_view.update(cx, |this, cx| this.open_bindings(id, window, cx));
                },
            ))
            .item(PopupMenuItem::new("Remove card").on_click(move |_, _, cx| {
                let _ = remove_view.update(cx, |this, cx| this.remove_card(id, cx));
            }))
        })
}

fn source_bar(
    id: PaneId,
    kind: Option<&CardKind>,
    page: &WorkspacePage,
    views: &CardViews<'_>,
    cx: &mut Context<RootView>,
) -> AnyElement {
    let Some(manifest) = kind.and_then(|kind| views.extensions.manifest(kind)) else {
        return div().into_any_element();
    };
    let bindings = page.workspace.bindings.get(&id);
    let mut bar = div()
        .id(SharedString::from(format!("sources-{}-{id}", page.id)))
        .flex()
        .items_center()
        .gap_1()
        .px_1()
        .min_h(px(28.))
        .flex_shrink_0()
        .overflow_x_scroll()
        .border_b_1()
        .border_color(cx.theme().border);
    let mut has_source = false;
    let mut unbound = vec![];
    for port in manifest.inputs {
        if let Some(binding) = bindings.and_then(|bindings| bindings.get(&port.id)) {
            has_source = true;
            let port_id = port.id.clone();
            let label = crate::ui::source_labels::source_label(views.backend, binding);
            bar = bar.child(
                Button::new(SharedString::from(format!("source-{id}-{}", port.id)))
                    .label(format!("{} · {label}", port.title))
                    .ghost()
                    .xsmall()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_card_source(id, Some(port_id.clone()), window, cx);
                    })),
            );
        } else {
            unbound.push((port.id, port.title));
        }
    }
    if !has_source {
        return div().into_any_element();
    }
    if !unbound.is_empty() {
        let view = cx.entity().downgrade();
        bar = bar.child(
            Button::new(("add-card-source", id))
                .label("+ Add source")
                .ghost()
                .xsmall()
                .dropdown_menu(move |mut menu, _, _| {
                    for (port, title) in &unbound {
                        let port = port.clone();
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(title.clone()).on_click(
                            move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| {
                                    this.choose_card_source(id, Some(port.clone()), window, cx)
                                });
                            },
                        ));
                    }
                    menu
                }),
        );
    }
    if views.advanced.visible && views.advanced.target == Some((page.id, id)) {
        bar = bar.child(
            Button::new(("close-card-editor", id))
                .label("Done")
                .ghost()
                .xsmall()
                .on_click(cx.listener(|this, _, _, cx| this.close_card_editors(cx))),
        );
    }
    bar.into_any_element()
}

fn card_panel(
    id: PaneId,
    kind: Option<&CardKind>,
    page: &WorkspacePage,
    views: &CardViews<'_>,
    cx: &mut Context<RootView>,
) -> impl IntoElement {
    let view = cx.entity().downgrade();
    let page_id = page.id;
    let extensions = views.extensions;
    let title = kind
        .map(|kind| extensions.title(kind))
        .unwrap_or_else(|| "Starter Pane".into());
    div()
        .id(SharedString::from(format!("page-{}-card-{id}", page.id)))
        .relative()
        .flex()
        .flex_col()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .border_1()
        .border_color(if page.workspace.selected == Some(id) {
            cx.theme().muted_foreground
        } else {
            cx.theme().border
        })
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| this.select_card(id, cx)),
        )
        .on_drag_move(
            cx.listener(move |this, event: &DragMoveEvent<CardDrag>, _, cx| {
                let drag = event.drag(cx);
                if drag.page == page_id {
                    this.drag_card(
                        page_id,
                        drag.card,
                        id,
                        event.event.position,
                        event.bounds,
                        cx,
                    );
                }
            }),
        )
        .child(
            div()
                .id(("card-top-bar", id))
                .flex()
                .items_center()
                .justify_between()
                .px_1()
                .h(px(28.))
                .flex_shrink_0()
                .bg(cx.theme().tab_bar)
                .child(
                    div()
                        .id(("card-drag-handle", id))
                        .flex()
                        .items_center()
                        .flex_1()
                        .h_full()
                        .cursor_grab()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .on_drag(
                            CardDrag {
                                page: page.id,
                                card: id,
                                title: title.clone().into(),
                            },
                            move |drag, _, _, cx| {
                                cx.stop_propagation();
                                let _ = view.update(cx, |this, cx| this.begin_card_drag(cx));
                                cx.new(|_| drag.clone())
                            },
                        )
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(card_menu(
                            id,
                            kind,
                            extensions,
                            page.workspace.bindings.get(&id),
                            cx,
                        )),
                ),
        )
        .child(source_bar(id, kind, page, views, cx))
        .child(
            if let Some((target, picker)) = &views.picker
                && *target == (page.id, id)
            {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(picker.clone())
                    .into_any_element()
            } else if views.advanced.visible && views.advanced.target == Some((page.id, id)) {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        Button::new(("finish-advanced", id))
                            .label("Back to card")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| this.close_card_editors(cx))),
                    )
                    .child(views.advanced.render(views.backend, cx))
                    .into_any_element()
            } else if let Some(manifest) = kind.and_then(|kind| extensions.manifest(kind))
                && manifest.inputs.iter().any(|port| {
                    port.required
                        && !page
                            .workspace
                            .bindings
                            .get(&id)
                            .is_some_and(|bindings| bindings.contains_key(&port.id))
                })
            {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .p_2()
                    .child(
                        Button::new(("choose-card-source", id))
                            .label("Choose data source")
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose_card_source(id, None, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Select a connection, then an instrument or dataset."),
                    )
                    .into_any_element()
            } else if matches!(kind, Some(CardKind::Native(_))) {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(views.native.render(page.id, id))
                    .into_any_element()
            } else if kind.and_then(CardKind::extension_id).is_some() {
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(views.web.render(page.id, id))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .p_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if kind.is_some() {
                        "This card's extension is not installed."
                    } else {
                        "Choose a view to get started"
                    })
                    .child(view_picker(id, kind, extensions, cx))
                    .into_any_element()
            },
        )
}
