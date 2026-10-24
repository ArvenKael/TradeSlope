use std::collections::{HashMap, HashSet};

use gpui_kit::*;
use tradeslope_native_sdk::{CardContext, NativeCardInstance};
use tradeslope_sdk::{BackendClient, Bindings, BoundInputs};

use crate::extensions::Extensions;
use crate::page::{
    CardKind, PageId, WorkspacePage,
    workspace::{Pane, PaneId},
};

struct Instance {
    kind: String,
    bindings: Bindings,
    card: Result<Box<dyn NativeCardInstance>, String>,
}

#[derive(Default)]
pub struct NativeCards {
    cards: HashMap<(PageId, PaneId), Instance>,
}

impl NativeCards {
    pub fn sync(
        &mut self,
        pages: &[WorkspacePage],
        visibility: &super::CardVisibility,
        extensions: &Extensions,
        backend: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        fn collect(pane: &Pane, into: &mut Vec<(PaneId, String)>) {
            match pane {
                Pane::Card {
                    id,
                    kind: Some(CardKind::Native(kind)),
                } => into.push((*id, kind.clone())),
                Pane::Split { first, second, .. } => {
                    collect(first, into);
                    collect(second, into);
                }
                _ => {}
            }
        }
        let mut live = HashSet::new();
        for page in pages {
            let mut ids = vec![];
            if let Some(root) = &page.workspace.root {
                collect(root, &mut ids);
            }
            for (id, kind) in ids {
                let key = (page.id, id);
                live.insert(key);
                let Some(factory) = extensions.native.get(&kind) else {
                    continue;
                };
                // Inactive native cards are temporarily unbound so their refresh
                // loops stop. The real mappings remain with their block.
                let active = visibility.is_active(page.id, id);
                let bindings = if active {
                    page.workspace
                        .bindings
                        .get(&id)
                        .cloned()
                        .unwrap_or_default()
                } else {
                    Bindings::new()
                };
                if self.cards.get(&key).is_some_and(|card| card.kind != kind) {
                    self.cards.remove(&key);
                }
                let needs_context = self
                    .cards
                    .get(&key)
                    .is_some_and(|card| card.bindings != bindings);
                if !active && !self.cards.contains_key(&key) {
                    continue;
                }
                if !self.cards.contains_key(&key) || needs_context {
                    let context = BoundInputs::new(
                        backend.clone(),
                        factory.manifest().inputs,
                        bindings.clone(),
                    )
                    .map(|inputs| CardContext {
                        id: format!("{}:{id}", page.id),
                        inputs,
                    });
                    if let (Some(instance), Ok(context)) =
                        (self.cards.get_mut(&key), context.as_ref())
                        && let Ok(card) = &mut instance.card
                    {
                        card.set_context(context.clone(), window, cx);
                        instance.bindings = bindings;
                    } else {
                        let card = context
                            .map(|context| factory.create(context, window, cx))
                            .map_err(|error| error.to_string());
                        self.cards.insert(
                            key,
                            Instance {
                                kind,
                                bindings,
                                card,
                            },
                        );
                    }
                }
            }
        }
        self.cards.retain(|key, _| live.contains(key));
    }

    pub fn render(&self, page: PageId, id: PaneId) -> AnyElement {
        match self.cards.get(&(page, id)).map(|instance| &instance.card) {
            Some(Ok(card)) => card.view().into_any_element(),
            Some(Err(error)) => div()
                .p_3()
                .child(format!("Input mapping invalid: {error}"))
                .into_any_element(),
            None => div()
                .p_3()
                .child("Native extension unavailable")
                .into_any_element(),
        }
    }
}
