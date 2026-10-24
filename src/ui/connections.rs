//! Schema-driven input setup, with manual mapping available as an advanced option.
use std::collections::BTreeMap;

use gpui_kit::base::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::*;
use serde_json::Value;
use tradeslope_sdk::{
    BackendClient, Bindings, CardManifest, Field, InputPort, Operation, PortBinding, Schema,
};

use super::root::RootView;
use crate::page::{PageId, workspace::PaneId};

pub struct ParameterDraft {
    pub field: Field,
    pub input: Entity<InputState>,
}

#[derive(Default)]
pub struct Connections {
    pub visible: bool,
    pub target: Option<(PageId, PaneId)>,
    pub card_id: String,
    pub title: String,
    pub ports: Vec<InputPort>,
    pub port: String,
    pub backend: String,
    pub operation: String,
    pub parameters: BTreeMap<String, ParameterDraft>,
    pub raw_parameters: Option<ParameterDraft>,
    pub fields: BTreeMap<String, String>,
    pub parameter_fields: BTreeMap<String, String>,
    pub status: String,
    pub show_schemas: bool,
}

impl Connections {
    pub fn sync(
        &mut self,
        target: Option<(PageId, PaneId)>,
        manifest: Option<CardManifest>,
        bindings: &Bindings,
        client: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (card_id, title, ports) = manifest
            .map(|manifest| (manifest.id, manifest.title, manifest.inputs))
            .unwrap_or_else(|| (String::new(), "Select a block".into(), vec![]));
        if self.same_selection(target, &card_id, &ports) {
            return;
        }
        self.target = target;
        self.card_id = card_id;
        self.title = title;
        self.ports = ports;
        self.port = self
            .ports
            .first()
            .map(|port| port.id.clone())
            .unwrap_or_default();
        self.select_port(self.port.clone(), bindings, client, window, cx);
    }

    fn same_selection(
        &self,
        target: Option<(PageId, PaneId)>,
        card_id: &str,
        ports: &[InputPort],
    ) -> bool {
        self.target == target && self.card_id == card_id && self.ports == ports
    }

    pub fn selected_port(&self) -> Option<&InputPort> {
        self.ports.iter().find(|port| port.id == self.port)
    }

    pub fn source(&self, client: &BackendClient) -> Option<Operation> {
        client
            .manifests()
            .into_iter()
            .find(|backend| backend.id == self.backend)?
            .operations
            .into_iter()
            .find(|operation| operation.id == self.operation)
    }

    pub fn select_port(
        &mut self,
        port: String,
        bindings: &Bindings,
        client: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.port = port;
        self.show_schemas = false;
        let binding = bindings.get(&self.port);
        self.backend = binding
            .map(|binding| binding.backend.clone())
            .unwrap_or_default();
        self.operation = binding
            .map(|binding| binding.operation.clone())
            .unwrap_or_default();
        self.fields = binding
            .map(|binding| binding.fields.clone())
            .unwrap_or_default();
        self.parameter_fields = binding
            .map(|binding| binding.parameter_fields.clone())
            .unwrap_or_default();
        self.make_parameters(binding.map(|binding| &binding.params), client, window, cx);
        self.status = if binding.is_some() {
            "Connected. Changes below are a draft until you apply them.".into()
        } else {
            "Unbound. Choose a source, fill in required arguments, then connect.".into()
        };
    }

    pub fn select_source(
        &mut self,
        backend: String,
        operation: String,
        client: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.backend = backend;
        self.operation = operation;
        self.parameter_fields = match (self.source(client), self.selected_port()) {
            (Some(source), Some(port)) => {
                exact_parameter_mapping(&port.parameters, &source.parameters)
            }
            _ => BTreeMap::new(),
        };
        self.fields = match (self.source(client), self.selected_port()) {
            (Some(source), Some(port)) => {
                tradeslope_sdk::suggest_output_mapping(&source.output, &port.output)
            }
            _ => BTreeMap::new(),
        };
        self.make_parameters(None, client, window, cx);
        self.status = "Source selected (draft). Exact matching fields are mapped automatically; apply to connect this input.".into();
    }

    fn make_parameters(
        &mut self,
        values: Option<&Value>,
        client: &BackendClient,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.parameters.clear();
        self.raw_parameters = None;
        if let Some(source) = self.source(client)
            && let Some(fields) = source.parameters.fields()
        {
            for (name, field) in fields {
                let value = values
                    .and_then(|values| values.get(name))
                    .or(field.default.as_ref());
                let text = value
                    .map(|value| edit_text(&field.schema, value))
                    .unwrap_or_default();
                self.parameters.insert(
                    name.clone(),
                    ParameterDraft {
                        field: field.clone(),
                        input: cx.new(|cx| InputState::new(window, cx).default_value(text)),
                    },
                );
            }
        } else if let Some(source) = self.source(client) {
            let text = values
                .map(|value| edit_text(&source.parameters, value))
                .unwrap_or_default();
            self.raw_parameters = Some(ParameterDraft {
                field: Field {
                    schema: source.parameters,
                    required: true,
                    description: String::new(),
                    default: None,
                },
                input: cx.new(|cx| InputState::new(window, cx).default_value(text)),
            });
        }
    }

    pub fn binding(&self, cx: &App) -> Result<PortBinding, String> {
        if self.backend.is_empty() || self.operation.is_empty() {
            return Err("Choose a compatible source first.".into());
        }
        if let Some(raw) = &self.raw_parameters {
            let text = raw.input.read(cx).value();
            let params = if text.trim().is_empty() {
                serde_json::json!({})
            } else {
                parse_value(&raw.field.schema, &text)?
            };
            return Ok(PortBinding {
                backend: self.backend.clone(),
                operation: self.operation.clone(),
                params,
                fields: self.fields.clone(),
                parameter_fields: BTreeMap::new(),
            });
        }
        let mut params = serde_json::Map::new();
        for (name, draft) in &self.parameters {
            if self.parameter_fields.contains_key(name) {
                continue;
            }
            let text = draft.input.read(cx).value();
            if let Some(value) = argument_value(name, &draft.field, &text)? {
                params.insert(name.clone(), value);
            }
        }
        Ok(PortBinding {
            backend: self.backend.clone(),
            operation: self.operation.clone(),
            params: Value::Object(params),
            fields: self.fields.clone(),
            parameter_fields: self.parameter_fields.clone(),
        })
    }

    pub fn render(&self, client: &BackendClient, cx: &mut Context<RootView>) -> AnyElement {
        let mut panel = div()
            .id("block-connections")
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .max_h(px(360.))
            .overflow_y_scroll()
            .flex_shrink_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .text_sm()
                    .child(format!("Advanced mapping · {}", self.title)),
            );
        let Some(port) = self.selected_port() else {
            return panel.child("Select a block with declared inputs. New blocks are not connected automatically.").into_any_element();
        };
        let view = cx.entity().downgrade();
        let port_choices = self.ports.clone();
        let mut sources = vec![];
        for backend in client.manifests() {
            for operation in backend.operations {
                if operation.kind == port.kind
                    && client.allows(operation.access)
                    && source_output_available(&operation.output, &port.output, self.show_schemas)
                {
                    sources.push((
                        backend.id.clone(),
                        operation.id.clone(),
                        format!("{} · {}", backend.title, operation.title),
                    ));
                }
            }
        }
        let source_label = sources
            .iter()
            .find(|(backend, operation, _)| {
                backend == &self.backend && operation == &self.operation
            })
            .map(|(_, _, title)| title.clone())
            .unwrap_or_else(|| {
                if self.backend.is_empty() {
                    format!("Choose compatible {:?} source", port.kind)
                } else {
                    "Selected source unavailable or incompatible".into()
                }
            });
        let validation = self.binding(cx).and_then(|binding| {
            tradeslope_sdk::validate_binding(client, port, &binding).map_err(|error| {
                format!("Check the arguments and Advanced mapping before applying: {error}")
            })
        });
        let source_view = view.clone();
        let ports = Button::new("input-port")
            .label(format!("Input: {}", port.title))
            .small()
            .dropdown_menu(move |mut menu, _, _| {
                for port in &port_choices {
                    let view = view.clone();
                    let id = port.id.clone();
                    menu = menu.item(PopupMenuItem::new(port.title.clone()).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.select_input_port(id.clone(), window, cx)
                            });
                        },
                    ));
                }
                menu
            });
        let source_button = Button::new("mapping-source")
            .label(source_label)
            .small()
            .dropdown_menu(move |mut menu, _, _| {
                if sources.is_empty() {
                    return menu
                        .item(PopupMenuItem::new("No compatible permitted source").disabled(true));
                }
                for (backend, operation, label) in &sources {
                    let view = source_view.clone();
                    let backend = backend.clone();
                    let operation = operation.clone();
                    menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
                        move |_, window, cx| {
                            let _ = view.update(cx, |this, cx| {
                                this.select_mapping_source(
                                    backend.clone(),
                                    operation.clone(),
                                    window,
                                    cx,
                                )
                            });
                        },
                    ));
                }
                menu
            });
        panel = panel.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(ports)
                .child(source_button)
                .child(
                    Button::new("apply-mapping")
                        .label("Apply connection")
                        .small()
                        .disabled(validation.is_err())
                        .on_click(cx.listener(|this, _, _, cx| this.apply_mapping(cx))),
                )
                .child(
                    Button::new("remove-mapping")
                        .label("Disconnect")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.remove_mapping(cx))),
                )
                .child(
                    Button::new("show-contracts")
                        .label(if self.show_schemas {
                            "Advanced mapping: on"
                        } else {
                            "Advanced mapping: off"
                        })
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.connections.show_schemas = !this.connections.show_schemas;
                            cx.notify();
                        })),
                ),
        );
        panel = panel.child(div().text_xs().child(validation.err().unwrap_or_else(|| {
            "Ready to apply. Runtime values, credentials and network requests may still fail."
                .into()
        })));
        if !self.parameters.is_empty() {
            panel = panel.child(div().text_xs().child("Operation arguments · * required"));
        }
        for (name, draft) in &self.parameters {
            panel = panel.child(self.parameter_row(name, draft, port, cx));
        }
        if let Some(raw) = &self.raw_parameters {
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .child(format!("Parameters · {}", type_name(&raw.field.schema))),
                    )
                    .child(div().w(px(280.)).child(Input::new(&raw.input).small()))
                    .child(
                        div()
                            .text_xs()
                            .child("Leave blank to use the block's declared arguments."),
                    ),
            );
        }
        if self.show_schemas
            && let Some(source) = self.source(client)
        {
            if let (Some(source_fields), Some(target_fields)) =
                (record_fields(&source.output), record_fields(&port.output))
            {
                panel = panel.child(
                    div()
                        .text_xs()
                        .child("Output fields: block field ← backend field"),
                );
                for (target, field) in target_fields {
                    let options: Vec<_> = source_fields
                        .iter()
                        .filter(|(_, source)| {
                            (!field.required || source.required)
                                && tradeslope_sdk::compatible_output(
                                    &source.schema,
                                    &field.schema,
                                    &BTreeMap::new(),
                                )
                        })
                        .map(|(name, _)| name.clone())
                        .collect();
                    let label = self.fields.get(target).cloned().unwrap_or_else(|| {
                        if options.contains(target) {
                            target.clone()
                        } else {
                            "Choose field".into()
                        }
                    });
                    let target_id = target.clone();
                    let view = cx.entity().downgrade();
                    panel = panel.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(170.)).text_xs().child(format!(
                                "{target}{}",
                                if field.required { " *" } else { "" }
                            )))
                            .child(
                                Button::new(SharedString::from(format!("field-{target}")))
                                    .label(label)
                                    .small()
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for option in &options {
                                            let view = view.clone();
                                            let target = target_id.clone();
                                            let source = option.clone();
                                            menu = menu.item(
                                                PopupMenuItem::new(option.clone()).on_click(
                                                    move |_, _, cx| {
                                                        let _ = view.update(cx, |this, cx| {
                                                            this.connections.fields.insert(
                                                                target.clone(),
                                                                source.clone(),
                                                            );
                                                            cx.notify();
                                                        });
                                                    },
                                                ),
                                            );
                                        }
                                        menu
                                    }),
                            ),
                    );
                }
            }
            if self.show_schemas {
                let schema = serde_json::json!({
                    "backend_parameters": source.parameters, "backend_output": source.output,
                    "block_parameters": port.parameters, "block_expected_output": port.output,
                    "kind": port.kind, "access": source.access,
                });
                panel = panel.child(
                    div()
                        .text_xs()
                        .child(serde_json::to_string_pretty(&schema).unwrap_or_default()),
                );
            }
        }
        panel
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.status.clone()),
            )
            .into_any_element()
    }

    fn parameter_row(
        &self,
        name: &str,
        draft: &ParameterDraft,
        port: &InputPort,
        cx: &mut Context<RootView>,
    ) -> AnyElement {
        let mut row = div().flex().flex_wrap().items_center().gap_2().child(
            div().w(px(170.)).text_xs().child(format!(
                "{name}{} · {}",
                if draft.field.required { " *" } else { "" },
                type_name(&draft.field.schema)
            )),
        );
        if self.show_schemas
            && let Some(fields) = port.parameters.fields()
        {
            let options: Vec<_> = fields
                .iter()
                .filter(|(_, source)| {
                    (!draft.field.required || source.required)
                        && tradeslope_sdk::compatible_parameters(
                            &source.schema,
                            &draft.field.schema,
                        )
                })
                .map(|(name, _)| name.clone())
                .collect();
            if !options.is_empty() {
                let view = cx.entity().downgrade();
                let parameter = name.to_string();
                let label = self
                    .parameter_fields
                    .get(name)
                    .map(|name| format!("From block: {name}"))
                    .unwrap_or_else(|| "Fixed value".into());
                row = row.child(
                    Button::new(SharedString::from(format!("param-source-{name}")))
                        .label(label)
                        .small()
                        .dropdown_menu(move |mut menu, _, _| {
                            let fixed_view = view.clone();
                            let fixed_parameter = parameter.clone();
                            menu = menu.item(PopupMenuItem::new("Fixed value").on_click(
                                move |_, _, cx| {
                                    let _ = fixed_view.update(cx, |this, cx| {
                                        this.connections.parameter_fields.remove(&fixed_parameter);
                                        cx.notify();
                                    });
                                },
                            ));
                            for input in &options {
                                let view = view.clone();
                                let parameter = parameter.clone();
                                let input = input.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(format!("From block: {input}")).on_click(
                                        move |_, _, cx| {
                                            let _ = view.update(cx, |this, cx| {
                                                this.connections
                                                    .parameter_fields
                                                    .insert(parameter.clone(), input.clone());
                                                cx.notify();
                                            });
                                        },
                                    ),
                                );
                            }
                            menu
                        }),
                );
            }
        }
        if !self.parameter_fields.contains_key(name) {
            if let Schema::Enum { values } = &draft.field.schema {
                let values = values.clone();
                let input = draft.input.clone();
                row = row.child(
                    Button::new(SharedString::from(format!("param-{name}")))
                        .label(draft.input.read(cx).value())
                        .small()
                        .dropdown_menu(move |mut menu, _, _| {
                            for value in &values {
                                let input = input.clone();
                                let value = value.clone();
                                menu = menu.item(PopupMenuItem::new(value.clone()).on_click(
                                    move |_, window, cx| {
                                        input.update(cx, |input, cx| {
                                            input.set_value(value.clone(), window, cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                );
            } else {
                row = row.child(div().w(px(190.)).child(Input::new(&draft.input).small()));
            }
        }
        if let Some(input) = self.parameter_fields.get(name) {
            row = row.child(div().text_xs().child(format!("Supplied by card: {input}")));
        }
        if !draft.field.description.is_empty() {
            row = row.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(draft.field.description.clone()),
            );
        }
        row.into_any_element()
    }
}

/// Normal setup offers only sources whose exact-name mapping satisfies the contract.
/// Advanced setup additionally permits sources that need an explicit field mapping.
fn source_output_available(source: &Schema, target: &Schema, advanced: bool) -> bool {
    tradeslope_sdk::compatible_output(
        source,
        target,
        &tradeslope_sdk::suggest_output_mapping(source, target),
    ) || (advanced && tradeslope_sdk::potentially_compatible(source, target))
}

fn exact_parameter_mapping(source: &Schema, target: &Schema) -> BTreeMap<String, String> {
    let (Some(source), Some(target)) = (source.fields(), target.fields()) else {
        return BTreeMap::new();
    };
    target
        .iter()
        .filter(|(name, target)| {
            source.get(*name).is_some_and(|source| {
                (!target.required || source.required)
                    && tradeslope_sdk::compatible_parameters(&source.schema, &target.schema)
            })
        })
        .map(|(name, _)| (name.clone(), name.clone()))
        .collect()
}

fn record_fields(schema: &Schema) -> Option<&BTreeMap<String, Field>> {
    tradeslope_sdk::output_fields(schema)
}

fn type_name(schema: &Schema) -> &'static str {
    match schema {
        Schema::String | Schema::Enum { .. } => "text",
        Schema::Number => "number",
        Schema::Integer => "integer",
        Schema::Boolean => "boolean",
        _ => "JSON",
    }
}

fn edit_text(schema: &Schema, value: &Value) -> String {
    if matches!(schema, Schema::String | Schema::Enum { .. }) {
        value.as_str().unwrap_or_default().into()
    } else {
        value.to_string()
    }
}

fn argument_value(name: &str, field: &Field, text: &str) -> Result<Option<Value>, String> {
    if text.trim().is_empty() {
        return if field.required {
            Err(format!("Enter a value for required argument “{name}”."))
        } else {
            Ok(None)
        };
    }
    parse_value(&field.schema, text)
        .map(Some)
        .map_err(|error| format!("{name}: {error}"))
}

fn parse_value(schema: &Schema, text: &str) -> Result<Value, String> {
    let value = if matches!(schema, Schema::String | Schema::Enum { .. }) {
        Value::String(text.into())
    } else {
        serde_json::from_str(text)
            .map_err(|_| "Enter a valid JSON value for this type.".to_string())?
    };
    schema.validate(&value).map_err(|error| error.to_string())?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{
        Connections, argument_value, edit_text, exact_parameter_mapping, parse_value,
        record_fields, source_output_available,
    };
    use serde_json::Value;
    use std::collections::BTreeMap;
    use tradeslope_sdk::{Field, Schema};

    fn object(fields: &[(&str, Schema, bool)]) -> Schema {
        Schema::Object {
            fields: fields
                .iter()
                .map(|(name, schema, required)| {
                    (
                        name.to_string(),
                        Field {
                            schema: schema.clone(),
                            required: *required,
                            description: String::new(),
                            default: None,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn automatic_arguments_use_only_safe_exact_names() {
        let source = object(&[
            ("symbol", Schema::String, true),
            ("limit", Schema::Integer, false),
            ("enabled", Schema::String, true),
            ("other", Schema::String, true),
        ]);
        let target = object(&[
            ("symbol", Schema::String, true),
            ("limit", Schema::Integer, true),
            ("enabled", Schema::Boolean, true),
            ("renamed", Schema::String, true),
        ]);
        assert_eq!(
            exact_parameter_mapping(&source, &target),
            BTreeMap::from([("symbol".into(), "symbol".into())])
        );
    }

    #[test]
    fn manual_rename_sources_require_advanced_mapping() {
        let source = object(&[("price", Schema::Number, true)]);
        let target = object(&[("last", Schema::Number, true)]);
        assert!(!source_output_available(&source, &target, false));
        assert!(source_output_available(&source, &target, true));
        assert!(source_output_available(&target, &target, false));
        let wrong_type = object(&[("last", Schema::String, true)]);
        assert!(!source_output_available(&wrong_type, &target, false));
    }

    #[test]
    fn argument_validation_explains_required_and_invalid_values() {
        let mut field = Field {
            schema: Schema::Integer,
            required: true,
            description: String::new(),
            default: None,
        };
        assert!(
            argument_value("limit", &field, " ")
                .unwrap_err()
                .contains("required argument “limit”")
        );
        assert!(
            argument_value("limit", &field, "1.5")
                .unwrap_err()
                .starts_with("limit:")
        );
        assert_eq!(
            argument_value("limit", &field, "0").unwrap(),
            Some(serde_json::json!(0))
        );
        field.required = false;
        assert_eq!(argument_value("limit", &field, "").unwrap(), None);
    }

    #[test]
    fn identical_ports_do_not_preserve_another_cards_draft() {
        let connections = Connections {
            card_id: "first.card".into(),
            ..Default::default()
        };
        assert!(connections.same_selection(None, "first.card", &[]));
        assert!(!connections.same_selection(None, "second.card", &[]));
    }

    #[test]
    fn nullable_records_offer_the_same_fields_as_sdk_mapping() {
        let schema = Schema::Nullable {
            inner: Box::new(Schema::Array {
                items: Box::new(tradeslope_sdk::ticker_schema()),
            }),
        };
        assert!(record_fields(&schema).unwrap().contains_key("last"));
    }

    #[test]
    fn parameter_editor_preserves_types_and_rejects_bad_json() {
        assert_eq!(
            parse_value(&Schema::String, "BTC_USDT").unwrap(),
            Value::String("BTC_USDT".into())
        );
        assert_eq!(
            parse_value(&Schema::Integer, "200").unwrap(),
            serde_json::json!(200)
        );
        assert!(parse_value(&Schema::Integer, "2.5").is_err());
        assert!(parse_value(&Schema::Boolean, "maybe").is_err());
        assert_eq!(
            edit_text(&Schema::String, &Value::String("hello".into())),
            "hello"
        );
    }
}
