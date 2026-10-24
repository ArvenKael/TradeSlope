//! Web documents receive only their block's declared, user-mapped input ports.
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};

use gpui_kit::component::ActiveTheme;
use gpui_kit::*;
use serde_json::{Value, json};
use tradeslope_sdk::{BackendClient, Bindings, BoundInputs};

use super::backend_bridge::{self, Bridge};
use crate::extensions::{Extensions, JavaScriptCard, package_file};
use crate::page::workspace::{Pane, PaneId};
use crate::page::{PageId, WorkspacePage};

struct WebCard {
    bridge: Result<Bridge, String>,
    kind: String,
    context: Value,
    bindings: Bindings,
    revisions: BTreeMap<String, u64>,
}

#[derive(Default)]
pub struct WebCards {
    cards: HashMap<(PageId, PaneId), WebCard>,
}

fn context(
    package: &JavaScriptCard,
    bindings: &Bindings,
    revisions: &BTreeMap<String, u64>,
    active: bool,
    theme: &str,
    backend: &BackendClient,
) -> Value {
    let inputs: serde_json::Map<_, _> = package
        .manifest
        .inputs
        .iter()
        .map(|port| {
            let (control_values, control_options) = bindings.get(&port.id)
                .map(|binding| super::source_labels::control_metadata(backend, port, binding))
                .unwrap_or_else(|| (json!({}), json!({})));
            (
                port.id.clone(),
                json!({
                    "connected": bindings.contains_key(&port.id),
                    "revision": revisions.get(&port.id).copied().unwrap_or(0),
                    "label": bindings.get(&port.id).map(|binding| super::source_labels::source_label(backend, binding)),
                    "controlValues": control_values,
                    "controlOptions": control_options,
                }),
            )
        })
        .collect();
    json!({"active":active,"theme":theme,"inputs":inputs})
}

fn create_bridge(
    package: &JavaScriptCard,
    inputs: BoundInputs,
    context: Value,
    window: &mut Window,
    cx: &mut App,
) -> Result<Bridge, String> {
    let builder = package_builder(package);
    backend_bridge::build(
        builder,
        format!("app://{}/", package.manifest.id),
        inputs,
        context,
        window,
        cx,
    )
}

impl WebCards {
    pub fn sync(
        &mut self,
        pages: &[WorkspacePage],
        visibility: &super::CardVisibility,
        extensions: &Extensions,
        backend: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        fn collect(pane: &Pane, cards: &mut Vec<(PaneId, String)>) {
            match pane {
                Pane::Card {
                    id,
                    kind: Some(kind),
                } => {
                    if let Some(kind) = kind.extension_id() {
                        cards.push((*id, kind.into()));
                    }
                }
                Pane::Split { first, second, .. } => {
                    collect(first, cards);
                    collect(second, cards);
                }
                _ => {}
            }
        }
        let theme = if cx.theme().mode.is_dark() {
            "dark"
        } else {
            "light"
        };
        let mut live = HashSet::new();
        for page in pages {
            let mut ids = vec![];
            if let Some(root) = &page.workspace.root {
                collect(root, &mut ids);
            }
            for (id, kind) in ids {
                let Some(package) = extensions.javascript.get(&kind) else {
                    continue;
                };
                let key = (page.id, id);
                live.insert(key);
                if self.cards.get(&key).is_some_and(|card| card.kind != kind) {
                    self.cards.remove(&key);
                }
                let bindings = page
                    .workspace
                    .bindings
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                let visible = visibility.is_active(page.id, id)
                    && package
                        .manifest
                        .inputs
                        .iter()
                        .filter(|port| port.required)
                        .all(|port| bindings.contains_key(&port.id));
                if visible {
                    self.cards.entry(key).or_insert_with(|| {
                        let revisions = BTreeMap::new();
                        let context = context(package, &bindings, &revisions, true, theme, backend);
                        let bridge = BoundInputs::new(
                            backend.clone(),
                            package.manifest.inputs.clone(),
                            bindings.clone(),
                        )
                        .map_err(|error| error.to_string())
                        .and_then(|inputs| {
                            create_bridge(package, inputs, context.clone(), window, cx)
                        });
                        WebCard {
                            bridge,
                            kind: kind.clone(),
                            context,
                            bindings: bindings.clone(),
                            revisions,
                        }
                    });
                }
                let Some(card) = self.cards.get_mut(&key) else {
                    continue;
                };
                if card.bindings != bindings {
                    for port in &package.manifest.inputs {
                        if card.bindings.get(&port.id) != bindings.get(&port.id) {
                            let revision = card.revisions.entry(port.id.clone()).or_default();
                            *revision = revision.wrapping_add(1);
                        }
                    }
                    let inputs = BoundInputs::new(
                        backend.clone(),
                        package.manifest.inputs.clone(),
                        bindings.clone(),
                    );
                    match inputs {
                        Ok(inputs) => match &card.bridge {
                            Ok(bridge) => bridge.replace_inputs(inputs),
                            Err(_) => {
                                let next = context(
                                    package,
                                    &bindings,
                                    &card.revisions,
                                    visible,
                                    theme,
                                    backend,
                                );
                                card.bridge = create_bridge(package, inputs, next, window, cx);
                            }
                        },
                        Err(error) => card.bridge = Err(error.to_string()),
                    }
                    card.bindings = bindings.clone();
                }
                let context = context(package, &bindings, &card.revisions, visible, theme, backend);
                if let Ok(bridge) = &card.bridge {
                    let view = &bridge.view;
                    if view.read(cx).visible() != visible {
                        view.update(cx, |view, cx| {
                            let _ = view.set_visible(visible);
                            cx.notify();
                        });
                    }
                    if card.context != context {
                        bridge.set_context(context.clone(), cx);
                    }
                }
                card.context = context;
            }
        }
        self.cards.retain(|key, _| live.contains(key));
    }

    pub fn render(&self, page: PageId, card: PaneId) -> AnyElement {
        match self.cards.get(&(page, card)).map(|card| &card.bridge) {
            Some(Ok(bridge)) => bridge.view.clone().into_any_element(),
            Some(Err(error)) => div()
                .p_3()
                .text_sm()
                .child(format!("Block input mapping unavailable: {error}"))
                .into_any_element(),
            None => div()
                .p_3()
                .child("JavaScript extension unavailable.")
                .into_any_element(),
        }
    }
}

fn package_builder(package: &JavaScriptCard) -> wry::WebViewBuilder<'static> {
    let root = package
        .directory
        .clone()
        .expect("external package has a directory");
    let url = format!("app://{}/{}", package.manifest.id, package.entry);
    let navigation = url.clone();
    wry::WebViewBuilder::new()
        .with_custom_protocol("app".into(), move |_, request| {
            let path = request.uri().path().trim_start_matches('/');
            let body = package_file(&root, path).and_then(|path| {
                if std::fs::metadata(&path).map_err(|error| error.to_string())?.len() > 8 * 1024 * 1024 {
                    return Err("Asset too large".into());
                }
                std::fs::read(path).map_err(|error| error.to_string())
            });
            let (status, body) = match body {
                Ok(bytes) => (200, Cow::Owned(bytes)),
                Err(_) => (404, Cow::Borrowed(b"Asset not found".as_slice())),
            };
            let mime = match path.rsplit('.').next() {
                Some("html") => "text/html; charset=utf-8",
                Some("js" | "mjs") => "text/javascript; charset=utf-8",
                Some("css") => "text/css; charset=utf-8",
                Some("svg") => "image/svg+xml",
                _ => "application/octet-stream",
            };
            wry::http::Response::builder().status(status).header("Content-Type", mime)
                .header("Content-Security-Policy", "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'none'; object-src 'none'; base-uri 'none'")
                .body(body).expect("static response headers")
        })
        .with_navigation_handler(move |url| url == navigation || url == navigation.replacen("app://", "http://app.", 1))
        .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
        .with_url(url)
}
