//! In-card, draft-only source selection. The owner applies emitted bindings.
use std::{
    collections::{BTreeMap, HashSet},
    sync::mpsc::TryRecvError,
    time::{Duration, Instant},
};

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::*;
use serde_json::{Map, Value};
use tradeslope_sdk::{
    Access, BackendClient, Bindings, Field, InputPort, PortBinding, Schema, SourceDescriptor,
    source_binding,
};

#[derive(Clone, Debug)]
pub enum SourcePickerEvent {
    Connect { port: String, binding: PortBinding },
    Cancel,
}

struct ConfigInput {
    field: Field,
    input: Entity<InputState>,
}

#[derive(Clone, Debug, PartialEq)]
struct CatalogItem {
    label: String,
    value: Value,
}

pub struct SourcePicker {
    client: BackendClient,
    ports: Vec<InputPort>,
    bindings: Bindings,
    port: String,
    sources: Vec<SourceDescriptor>,
    allowed_backends: Option<HashSet<String>>,
    selected: Option<SourceDescriptor>,
    config: BTreeMap<String, ConfigInput>,
    search: Entity<InputState>,
    results_scroll: UniformListScrollHandle,
    items: Vec<CatalogItem>,
    selected_item: Option<CatalogItem>,
    loading: bool,
    catalog_error: Option<String>,
    error: Option<String>,
    generation: u64,
    catalog_task: Option<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<SourcePickerEvent> for SourcePicker {}

impl SourcePicker {
    pub fn new(
        client: BackendClient,
        ports: Vec<InputPort>,
        bindings: Bindings,
        initial_port: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let port = initial_port
            .filter(|id| ports.iter().any(|port| &port.id == id))
            .or_else(|| ports.first().map(|port| port.id.clone()))
            .unwrap_or_default();
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search instruments or datasets"));
        let subscription = cx.subscribe(&search, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.error = None;
                this.results_scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }
        });
        let sources = client.sources();
        let mut picker = Self {
            client,
            ports,
            bindings,
            port,
            sources,
            allowed_backends: None,
            selected: None,
            config: BTreeMap::new(),
            search,
            results_scroll: UniformListScrollHandle::new(),
            items: vec![],
            selected_item: None,
            loading: false,
            catalog_error: None,
            error: None,
            generation: 0,
            catalog_task: None,
            subscriptions: vec![subscription],
        };
        picker.preload(window, cx);
        picker
    }

    /// Supply enabled/available providers from application settings. `None` allows
    /// all descriptors permitted by the client; `Some(empty)` allows none.
    pub fn set_allowed_backends(
        &mut self,
        allowed: Option<HashSet<String>>,
        cx: &mut Context<Self>,
    ) {
        self.allowed_backends = allowed;
        if self
            .selected
            .as_ref()
            .is_some_and(|source| !self.allowed(source))
        {
            self.reset();
        }
        cx.notify();
    }

    fn allowed(&self, source: &SourceDescriptor) -> bool {
        self.allowed_backends
            .as_ref()
            .is_none_or(|ids| ids.contains(&source.backend))
    }

    fn port(&self) -> Option<&InputPort> {
        self.ports.iter().find(|port| port.id == self.port)
    }

    fn compatible(&self, source: &SourceDescriptor) -> bool {
        self.allowed(source)
            && self
                .port()
                .is_some_and(|port| compatible_source(&self.client, port, source))
    }

    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        // Dropping the task cancels polling; the SDK may already have dispatched
        // the request, but its eventual result cannot update this draft.
        self.catalog_task = None;
        self.loading = false;
        self.selected = None;
        self.config.clear();
        self.subscriptions.truncate(1);
        self.items.clear();
        self.results_scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.selected_item = None;
        self.catalog_error = None;
        self.error = None;
    }

    fn preload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(binding) = self.bindings.get(&self.port).cloned() else {
            return;
        };
        let source = self
            .sources
            .iter()
            .find(|source| self.compatible(source) && matches_binding(source, &binding))
            .cloned();
        if let Some(source) = source {
            self.select_source(source, Some(binding.params), window, cx);
        }
    }

    fn select_source(
        &mut self,
        source: SourceDescriptor,
        previous: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset();
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        if source.source.catalog.is_some() {
            self.search.read(cx).focus_handle(cx).focus(window, cx);
        }
        let defaults = default_config(&source.source.configuration);
        let values = previous.as_ref().unwrap_or(&defaults);
        if let Some(catalog) = &source.source.catalog
            && let Some(value) = values.get(&catalog.parameter)
        {
            self.selected_item = Some(CatalogItem {
                label: display_value(value),
                value: value.clone(),
            });
        }
        if let Some(fields) = source.source.configuration.fields() {
            for (name, field) in fields {
                if source.source.controls.contains_key(name)
                    || source
                        .source
                        .catalog
                        .as_ref()
                        .is_some_and(|c| c.parameter == *name)
                {
                    continue;
                }
                let text = values
                    .get(name)
                    .or_else(|| defaults.get(name))
                    .map(|value| edit_value(&field.schema, value))
                    .unwrap_or_default();
                let input = cx.new(|cx| InputState::new(window, cx).default_value(text));
                self.subscriptions
                    .push(cx.subscribe(&input, |this, _, event, cx| {
                        if matches!(event, InputEvent::Change) {
                            this.error = None;
                            cx.notify();
                        }
                    }));
                self.config.insert(
                    name.clone(),
                    ConfigInput {
                        field: field.clone(),
                        input,
                    },
                );
            }
        }
        let has_catalog = source.source.catalog.is_some();
        self.selected = Some(source);
        if has_catalog {
            self.load_catalog(cx);
        }
        cx.notify();
    }

    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.selected.clone() else {
            return;
        };
        let Some(catalog) = source.source.catalog.clone() else {
            return;
        };
        // Only typed, public queries may populate a chooser.
        let operation = self
            .client
            .manifests()
            .into_iter()
            .find(|backend| backend.id == source.backend)
            .and_then(|backend| {
                backend
                    .operations
                    .into_iter()
                    .find(|op| op.id == catalog.operation)
            });
        if !operation.is_some_and(|op| {
            op.access == Access::Public
                && op.kind == tradeslope_sdk::OperationKind::Query
                && catalog_schema(&op.output, &catalog.value_field, &catalog.label_field)
        }) {
            self.catalog_error =
                Some("This source does not declare a typed public catalog.".into());
            cx.notify();
            return;
        }
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        self.catalog_task = None;
        self.loading = true;
        self.catalog_error = None;
        let client = self.client.clone();
        self.catalog_task = Some(cx.spawn(async move |weak, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client
                        .submit(&source.backend, &catalog.operation, catalog.params.clone())
                        .map(|pending| (pending, catalog))
                        .map_err(|error| error.to_string())
                })
                .await;
            let result = match result {
                Err(error) => Err(error),
                Ok((pending, catalog)) => {
                    let deadline = Instant::now() + Duration::from_secs(30);
                    loop {
                        let current = weak.update(cx, |this, _| this.generation == generation);
                        if !matches!(current, Ok(true)) {
                            return;
                        }
                        match pending.try_recv() {
                            Ok(result) => {
                                break cx
                                    .background_executor()
                                    .spawn(async move {
                                        result.map_err(|error| error.to_string()).and_then(
                                            |value| {
                                                catalog_items(
                                                    &value,
                                                    &catalog.value_field,
                                                    &catalog.label_field,
                                                )
                                            },
                                        )
                                    })
                                    .await;
                            }
                            Err(TryRecvError::Disconnected) => {
                                break Err("Catalog service disconnected.".into());
                            }
                            Err(TryRecvError::Empty) if Instant::now() >= deadline => {
                                break Err("Catalog request timed out.".into());
                            }
                            Err(TryRecvError::Empty) => {
                                cx.background_executor()
                                    .timer(Duration::from_millis(40))
                                    .await;
                            }
                        }
                    }
                }
            };
            let _ = weak.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(items) => {
                        this.selected_item = this
                            .selected_item
                            .as_ref()
                            .and_then(|selected| catalog_selection(&items, &selected.value));
                        this.items = items;
                    }
                    Err(error) => this.catalog_error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    fn binding(&self, cx: &App) -> Result<PortBinding, String> {
        let source = self.selected.as_ref().ok_or("Choose a provider first.")?;
        if !self.compatible(source) {
            return Err("This source is no longer available for this input.".into());
        }
        let port = self.port().ok_or("Choose an input.")?;
        let mut config = Map::new();
        for (name, draft) in &self.config {
            let text = draft.input.read(cx).value();
            if text.is_empty() {
                if let Some(default) = &draft.field.default {
                    config.insert(name.clone(), default.clone());
                    continue;
                }
                if !draft.field.required {
                    continue;
                }
                return Err(format!("Enter {}.", friendly_name(name)));
            }
            config.insert(
                name.clone(),
                parse_config(&draft.field.schema, &text)
                    .map_err(|error| format!("{}: {error}", friendly_name(name)))?,
            );
        }
        if let Some(catalog) = &source.source.catalog {
            if self.loading {
                return Err("Wait for the catalog to load.".into());
            }
            if self.catalog_error.is_some() {
                return Err("Retry the catalog before connecting.".into());
            }
            let selected = self
                .selected_item
                .as_ref()
                .ok_or("Select an instrument or dataset.")?;
            if !self.items.iter().any(|item| item.value == selected.value) {
                return Err("Select an item from the current catalog.".into());
            }
            config.insert(catalog.parameter.clone(), selected.value.clone());
        }
        source_binding(
            &self.client,
            port,
            &source.backend,
            &source.source.id,
            Value::Object(config),
        )
        .map_err(|error| error.to_string())
    }
}

impl Render for SourcePicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_catalog = self
            .selected
            .as_ref()
            .is_some_and(|source| source.source.catalog.is_some());
        let frame = div()
            .id("source-picker")
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().child("Choose data source"))
                    .child(
                        Button::new("close-picker")
                            .label("Close")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reset();
                                cx.emit(SourcePickerEvent::Cancel);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().child(if has_catalog {
                        "Search all instruments or datasets"
                    } else {
                        "Choose a provider below to search its instruments"
                    }))
                    .child(Input::new(&self.search).small().disabled(!has_catalog)),
            );
        let mut body = div()
            .id("source-picker-body")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .overflow_y_scroll();
        if self.port().is_none() {
            return frame
                .child(body.child("This card has no data inputs."))
                .into_any_element();
        }
        let ports = self.ports.clone();
        let weak = cx.entity().downgrade();
        body = body.child(
            Button::new("picker-role")
                .label(
                    self.port()
                        .map(|port| port.title.clone())
                        .unwrap_or_default(),
                )
                .small()
                .dropdown_menu(move |mut menu, _, _| {
                    for port in &ports {
                        let weak = weak.clone();
                        let id = port.id.clone();
                        menu = menu.item(PopupMenuItem::new(port.title.clone()).on_click(
                            move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.reset();
                                    this.port = id.clone();
                                    this.preload(window, cx);
                                    cx.notify();
                                });
                            },
                        ));
                    }
                    menu
                }),
        );
        let sources: Vec<_> = self
            .sources
            .iter()
            .filter(|source| self.compatible(source))
            .cloned()
            .collect();
        if sources.is_empty() {
            body = body.child("No compatible enabled sources. Manage connections in App Settings.");
        }
        let label = self
            .selected
            .as_ref()
            .map(|source| format!("{} · {}", source.backend_title, source.source.title))
            .unwrap_or_else(|| "Choose provider".into());
        let weak = cx.entity().downgrade();
        body = body.child(
            Button::new("picker-provider")
                .label(label)
                .small()
                .dropdown_menu(move |mut menu, _, _| {
                    for source in &sources {
                        let weak = weak.clone();
                        let source = source.clone();
                        menu = menu.item(
                            PopupMenuItem::new(format!(
                                "{} · {}",
                                source.backend_title, source.source.title
                            ))
                            .on_click(move |_, window, cx| {
                                let _ = weak.update(cx, |this, cx| {
                                    this.select_source(source.clone(), None, window, cx)
                                });
                            }),
                        );
                    }
                    menu
                }),
        );
        if self
            .selected
            .as_ref()
            .is_some_and(|source| source.source.catalog.is_some())
        {
            if let Some(selected) = &self.selected_item {
                body = body.child(
                    div()
                        .text_sm()
                        .child(format!("Selected: {}", selected.label)),
                );
            }
            if self.loading {
                body = body.child("Loading catalog…");
            }
            if let Some(error) = &self.catalog_error {
                body = body.child(div().text_sm().child(error.clone())).child(
                    Button::new("retry-catalog")
                        .label("Retry")
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.load_catalog(cx))),
                );
            } else if !self.loading {
                let query = self.search.read(cx).value().to_lowercase();
                let matches = matching_catalog_indices(&self.items, &query);
                body = body.child(div().text_xs().child(format!(
                    "{} matches in {} instruments · scroll to browse all",
                    matches.len(),
                    self.items.len(),
                )));
                if matches.is_empty() {
                    body = body.child("No matches. Try another name or symbol.");
                } else {
                    let height = px((matches.len().min(6) as f32) * 32.);
                    body = body.child(
                        uniform_list(
                            "catalog-results",
                            matches.len(),
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .filter_map(|index| {
                                        let item = this.items.get(*matches.get(index)?)?.clone();
                                        Some(
                                            div()
                                                .id(("catalog-item", index))
                                                .h(px(32.))
                                                .w_full()
                                                .child(
                                                    Button::new(("select-catalog-item", index))
                                                        .label(item.label.clone())
                                                        .w_full()
                                                        .small()
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.selected_item =
                                                                    Some(item.clone());
                                                                this.error = None;
                                                                cx.notify();
                                                            },
                                                        )),
                                                ),
                                        )
                                    })
                                    .collect()
                            }),
                        )
                        .track_scroll(&self.results_scroll)
                        .h(height)
                        .w_full()
                        .flex_shrink_0(),
                    );
                }
            }
        }
        for (name, draft) in &self.config {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().child(format!(
                        "{}{}",
                        friendly_name(name),
                        if draft.field.required { " *" } else { "" }
                    )))
                    .child(Input::new(&draft.input).small())
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(draft.field.description.clone()),
                    ),
            );
        }
        let validation = self.binding(cx);
        if let Some(error) = self.error.as_ref().or_else(|| validation.as_ref().err()) {
            body = body.child(div().text_sm().child(error.clone()));
        }
        frame
            .child(body)
            .child(
                div().flex_shrink_0().child(
                    Button::new("connect-source")
                        .label("Connect")
                        .small()
                        .disabled(validation.is_err())
                        .on_click(cx.listener(|this, _, _, cx| match this.binding(cx) {
                            Ok(binding) => {
                                this.generation = this.generation.wrapping_add(1);
                                this.catalog_task = None;
                                cx.emit(SourcePickerEvent::Connect {
                                    port: this.port.clone(),
                                    binding,
                                });
                            }
                            Err(error) => {
                                this.error = Some(error);
                                cx.notify();
                            }
                        })),
                ),
            )
            .into_any_element()
    }
}

fn default_config(schema: &Schema) -> Value {
    Value::Object(
        schema
            .fields()
            .into_iter()
            .flatten()
            .filter_map(|(name, field)| {
                field
                    .default
                    .as_ref()
                    .map(|value| (name.clone(), value.clone()))
            })
            .collect(),
    )
}

fn matches_binding(source: &SourceDescriptor, binding: &PortBinding) -> bool {
    source.backend == binding.backend
        && source.source.operation == binding.operation
        && source.source.params.as_object().is_some_and(|params| {
            params
                .iter()
                .filter(|(name, _)| {
                    !source.source.controls.contains_key(*name)
                        && !source
                            .source
                            .configuration
                            .fields()
                            .is_some_and(|fields| fields.contains_key(*name))
                })
                .all(|(name, value)| binding.params.get(name) == Some(value))
        })
}

fn compatible_source(client: &BackendClient, port: &InputPort, source: &SourceDescriptor) -> bool {
    if source_binding(
        client,
        port,
        &source.backend,
        &source.source.id,
        default_config(&source.source.configuration),
    )
    .is_ok()
    {
        return true;
    }
    // Missing user configuration must not hide an otherwise compatible source.
    // Reject invalid defaults and controls instead of fabricating required values.
    let Some(fields) = source.source.configuration.fields() else {
        return false;
    };
    if fields.values().any(|field| {
        field
            .default
            .as_ref()
            .is_some_and(|value| field.schema.validate(value).is_err())
    }) {
        return false;
    }
    let Some(operation) = client
        .manifests()
        .into_iter()
        .find(|backend| backend.id == source.backend)
        .and_then(|backend| {
            backend
                .operations
                .into_iter()
                .find(|op| op.id == source.source.operation)
        })
    else {
        return false;
    };
    operation.kind == port.kind
        && client.allows(operation.access)
        && tradeslope_sdk::compatible_output(&operation.output, &port.output, &BTreeMap::new())
        && source.source.controls.iter().all(|(backend, frontend)| {
            let target = operation
                .parameters
                .fields()
                .and_then(|fields| fields.get(backend));
            let input = port
                .parameters
                .fields()
                .and_then(|fields| fields.get(frontend));
            matches!((target, input), (Some(target), Some(input))
                if (!target.required || input.required || target.default.is_some())
                    && tradeslope_sdk::compatible_parameters(&input.schema, &target.schema))
        })
}

fn catalog_schema(schema: &Schema, value: &str, label: &str) -> bool {
    let Schema::Array { items } = schema else {
        return false;
    };
    items.fields().is_some_and(|fields| {
        fields.contains_key(value)
            && fields
                .get(label)
                .is_some_and(|field| matches!(field.schema, Schema::String | Schema::Enum { .. }))
    })
}

fn catalog_items(
    value: &Value,
    value_field: &str,
    label_field: &str,
) -> Result<Vec<CatalogItem>, String> {
    let rows = value.as_array().ok_or("Catalog must return a list.")?;
    rows.iter()
        .map(|row| {
            let value = row
                .get(value_field)
                .ok_or("Catalog item is missing its value.")?
                .clone();
            let label = row
                .get(label_field)
                .and_then(Value::as_str)
                .ok_or("Catalog item is missing its title.")?
                .to_owned();
            Ok(CatalogItem { label, value })
        })
        .collect()
}

fn matches_query(item: &CatalogItem, query: &str) -> bool {
    let query = normalized_search(query);
    normalized_search(&item.label).contains(&query)
        || normalized_search(&display_value(&item.value)).contains(&query)
}

fn matching_catalog_indices(items: &[CatalogItem], query: &str) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| matches_query(item, query).then_some(index))
        .collect()
}

/// Normalize legacy/default aliases only when exactly one catalog row matches.
fn catalog_selection(items: &[CatalogItem], value: &Value) -> Option<CatalogItem> {
    if let Some(item) = items.iter().find(|item| &item.value == value) {
        return Some(item.clone());
    }
    let key = normalized_search(value.as_str()?);
    if key.is_empty() {
        return None;
    }
    let mut matches = items.iter().filter(|item| {
        item.value
            .as_str()
            .is_some_and(|value| normalized_search(value) == key)
    });
    let item = matches.next()?;
    if matches.next().is_some() {
        None
    } else {
        Some(item.clone())
    }
}

fn normalized_search(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn display_value(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn edit_value(schema: &Schema, value: &Value) -> String {
    if matches!(schema, Schema::String | Schema::Enum { .. }) {
        display_value(value)
    } else {
        value.to_string()
    }
}

fn parse_config(schema: &Schema, text: &str) -> Result<Value, String> {
    let value = if matches!(schema, Schema::String | Schema::Enum { .. }) {
        Value::String(text.into())
    } else {
        serde_json::from_str(text).map_err(|_| "Enter a valid JSON value.".to_string())?
    };
    schema.validate(&value).map_err(|error| error.to_string())?;
    Ok(value)
}

fn friendly_name(name: &str) -> String {
    let text = name.replace(['_', '-'], " ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        catalog_items, catalog_selection, default_config, matches_binding, matches_query,
        matching_catalog_indices, parse_config,
    };
    use serde_json::json;
    use std::collections::BTreeMap;
    use tradeslope_sdk::{DataSource, Field, PortBinding, Schema, SourceDescriptor};

    #[test]
    fn all_catalog_rows_remain_browsable_and_searchable_beyond_one_hundred() {
        let rows: Vec<_> = (0..350)
            .map(|index| {
                json!({
                    "code": format!("PAIR{index}_USDT"), "title": format!("Market {index}"),
                })
            })
            .collect();
        let items = catalog_items(&json!(rows), "code", "title").unwrap();
        let all = matching_catalog_indices(&items, "");
        assert_eq!(all.len(), 350);
        assert_eq!(all[349], 349);
        for query in ["PAIR349/USDT", "pair349 usdt", "Market 349"] {
            let found = matching_catalog_indices(&items, query);
            assert_eq!(found, vec![349]);
            assert_eq!(items[found[0]].value, json!("PAIR349_USDT"));
        }
        assert!(matching_catalog_indices(&items, "missing").is_empty());
    }

    #[test]
    fn default_selection_must_exist_in_successful_catalog() {
        assert!(catalog_selection(&[], &json!("ABC_XYZ")).is_none());
        let items = catalog_items(
            &json!([{"code":"ABCXYZ","title":"Example"}]),
            "code",
            "title",
        )
        .unwrap();
        assert_eq!(
            catalog_selection(&items, &json!("ABC/XYZ")).unwrap().value,
            json!("ABCXYZ")
        );
        assert!(catalog_selection(&items, &json!("missing")).is_none());
        let ambiguous = catalog_items(
            &json!([
                {"code":"ABCXYZ","title":"One"}, {"code":"ABC_XYZ","title":"Two"}
            ]),
            "code",
            "title",
        )
        .unwrap();
        assert!(catalog_selection(&ambiguous, &json!("ABC/XYZ")).is_none());
    }

    #[test]
    fn existing_binding_matches_fixed_identity_not_dynamic_controls() {
        let source = SourceDescriptor {
            backend: "provider".into(),
            backend_title: "Provider".into(),
            source: DataSource {
                id: "dataset".into(),
                title: "Dataset".into(),
                operation: "read".into(),
                params: json!({"market": "spot", "period": "short"}),
                configuration: Schema::object(BTreeMap::new()),
                controls: BTreeMap::from([("period".into(), "period".into())]),
                catalog: None,
            },
        };
        let mut binding = PortBinding {
            backend: "provider".into(),
            operation: "read".into(),
            params: json!({"market": "spot"}),
            fields: BTreeMap::new(),
            parameter_fields: source.source.controls.clone(),
        };
        assert!(matches_binding(&source, &binding));
        binding.params = json!({"market": "other"});
        assert!(!matches_binding(&source, &binding));
        binding.params = json!({"market": "spot"});
        binding.backend = "other".into();
        assert!(!matches_binding(&source, &binding));
    }

    #[test]
    fn defaults_do_not_invent_required_configuration() {
        let schema = Schema::object(BTreeMap::from([
            (
                "dataset".into(),
                Field {
                    schema: Schema::String,
                    required: true,
                    description: String::new(),
                    default: None,
                },
            ),
            (
                "count".into(),
                Field {
                    schema: Schema::Integer,
                    required: true,
                    description: String::new(),
                    default: Some(json!(10)),
                },
            ),
        ]));
        assert_eq!(default_config(&schema), json!({"count": 10}));
    }

    #[test]
    fn catalog_uses_descriptor_fields_and_searches_labels_and_values() {
        let items = catalog_items(
            &json!([{"code": 17, "title": "Example dataset"}]),
            "code",
            "title",
        )
        .unwrap();
        assert!(matches_query(&items[0], "example"));
        assert!(matches_query(&items[0], "17"));
        assert!(!matches_query(&items[0], "missing"));
        let symbols = catalog_items(
            &json!([{"code": "ABC_XYZ", "title": "Example"}]),
            "code",
            "title",
        )
        .unwrap();
        for query in ["abc/xyz", "ABC XYZ", "abc_xyz"] {
            assert!(matches_query(&symbols[0], query));
        }
        assert!(catalog_items(&json!([{"code": 17}]), "code", "title").is_err());
        assert!(catalog_items(&json!({}), "code", "title").is_err());
    }

    #[test]
    fn configuration_keeps_types_and_checks_enums() {
        assert_eq!(parse_config(&Schema::String, "001").unwrap(), json!("001"));
        assert_eq!(parse_config(&Schema::Integer, "12").unwrap(), json!(12));
        assert!(parse_config(&Schema::Integer, "1.2").is_err());
        assert!(
            parse_config(
                &Schema::Enum {
                    values: vec!["daily".into()]
                },
                "other"
            )
            .is_err()
        );
    }
}
