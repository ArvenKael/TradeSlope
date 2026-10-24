//! Presentation derived from registered datasets, never exchange-specific names.
use tradeslope_sdk::{BackendClient, InputPort, PortBinding, Schema, SourceDescriptor};

pub(crate) fn descriptor_for_binding(
    client: &BackendClient,
    binding: &PortBinding,
) -> Option<SourceDescriptor> {
    client.sources().into_iter().find(|descriptor| {
        descriptor.backend == binding.backend
            && descriptor.source.operation == binding.operation
            && descriptor.source.params.as_object().is_some_and(|params| {
                params.iter().all(|(key, value)| {
                    descriptor.source.controls.contains_key(key)
                        || binding.params.get(key) == Some(value)
                })
            })
    })
}

pub(crate) fn source_label(client: &BackendClient, binding: &PortBinding) -> String {
    if let Some(descriptor) = descriptor_for_binding(client, binding) {
        let dataset = descriptor
            .source
            .catalog
            .as_ref()
            .and_then(|catalog| binding.params.get(&catalog.parameter))
            .and_then(serde_json::Value::as_str);
        let mut label = format!("{} · {}", descriptor.backend_title, descriptor.source.title);
        if let Some(dataset) = dataset {
            label.push_str(" · ");
            label.push_str(dataset);
        }
        label
    } else {
        client
            .manifests()
            .into_iter()
            .find(|manifest| manifest.id == binding.backend)
            .map(|manifest| format!("{} · Custom source", manifest.title))
            .unwrap_or_else(|| "Unavailable source".into())
    }
}

/// Describe the effective routing, including advanced constants that override
/// dynamic values. A chart must never label fixed daily data as five-minute data.
pub(crate) fn control_metadata(
    client: &BackendClient,
    port: &InputPort,
    binding: &PortBinding,
) -> (serde_json::Value, serde_json::Value) {
    use serde_json::{Map, Value, json};
    let mut values = Map::new();
    let mut options = Map::new();
    let operation = client
        .manifests()
        .into_iter()
        .find(|manifest| manifest.id == binding.backend)
        .and_then(|manifest| {
            manifest
                .operations
                .into_iter()
                .find(|operation| operation.id == binding.operation)
        });
    let Some(operation) = operation else {
        return (json!({}), json!({}));
    };
    let declared = descriptor_for_binding(client, binding);
    for (name, field) in port.parameters.fields().into_iter().flatten() {
        let mut targets: Vec<_> = binding
            .parameter_fields
            .iter()
            .filter(|(_, from)| *from == name)
            .map(|(to, _)| to.clone())
            .collect();
        if targets.is_empty() {
            if let Some(source) = &declared {
                targets.extend(
                    source
                        .source
                        .controls
                        .iter()
                        .filter(|(_, from)| *from == name)
                        .map(|(to, _)| to.clone()),
                );
            }
            if targets.is_empty()
                && operation
                    .parameters
                    .fields()
                    .is_some_and(|fields| fields.contains_key(name))
            {
                targets.push(name.clone());
            }
        }
        let fixed: Vec<_> = targets
            .iter()
            .filter_map(|target| binding.params.get(target))
            .collect();
        if let Some(value) = fixed.first() {
            if fixed.iter().all(|candidate| *candidate == *value) {
                values.insert(name.clone(), (*value).clone());
                options.insert(name.clone(), json!([value]));
            } else {
                options.insert(name.clone(), json!([]));
            }
            continue;
        }
        let dynamic: Vec<_> = binding
            .parameter_fields
            .iter()
            .filter(|(_, from)| *from == name)
            .map(|(to, _)| to)
            .collect();
        if dynamic.is_empty() {
            options.insert(name.clone(), json!([]));
        } else if let Schema::Enum { values: choices } = &field.schema {
            let choices: Vec<_> = choices
                .iter()
                .filter(|choice| {
                    dynamic.iter().all(|target| {
                        operation
                            .parameters
                            .fields()
                            .and_then(|fields| fields.get(*target))
                            .is_some_and(|target| {
                                target
                                    .schema
                                    .validate(&Value::String((*choice).clone()))
                                    .is_ok()
                            })
                    })
                })
                .collect();
            options.insert(name.clone(), json!(choices));
        }
    }
    (Value::Object(values), Value::Object(options))
}

#[cfg(test)]
mod tests {
    use super::control_metadata;
    use serde_json::{Value, json};
    use std::{collections::BTreeMap, sync::Arc};
    use tradeslope_sdk::{
        Backend, BackendHost, BackendManifest, BackendResult, Field, InputPort, Operation,
        OperationKind, PortBinding, Schema,
    };

    struct Fixture;
    impl Backend for Fixture {
        fn manifest(&self) -> BackendManifest {
            BackendManifest {
                id: "source".into(),
                title: "Source".into(),
                version: "1".into(),
                operations: vec![Operation::query(
                    "read",
                    "Read",
                    Schema::object(BTreeMap::from([(
                        "interval".into(),
                        Field {
                            schema: Schema::String,
                            required: true,
                            description: String::new(),
                            default: None,
                        },
                    )])),
                    Schema::Any,
                )],
            }
        }
        fn execute(&self, _: &str, _: Value) -> BackendResult<Value> {
            panic!("control metadata must never execute an operation")
        }
    }

    #[test]
    fn effective_controls_distinguish_fixed_dynamic_and_unrouted_intervals() {
        let host = BackendHost::new(vec![Arc::new(Fixture)]).unwrap();
        let client = host.client(Default::default());
        let port = InputPort {
            id: "bars".into(),
            title: "Main price".into(),
            kind: OperationKind::Query,
            parameters: Schema::object(BTreeMap::from([(
                "interval".into(),
                Field {
                    schema: Schema::Enum {
                        values: vec!["5m".into(), "1h".into(), "1d".into()],
                    },
                    required: true,
                    description: String::new(),
                    default: None,
                },
            )])),
            output: Schema::Any,
            required: true,
        };
        let mut binding = PortBinding {
            backend: "source".into(),
            operation: "read".into(),
            params: json!({"interval":"1d"}),
            fields: BTreeMap::new(),
            parameter_fields: BTreeMap::new(),
        };
        assert_eq!(
            control_metadata(&client, &port, &binding),
            (json!({"interval":"1d"}), json!({"interval":["1d"]}))
        );
        binding
            .parameter_fields
            .insert("interval".into(), "interval".into());
        assert_eq!(
            control_metadata(&client, &port, &binding).0,
            json!({"interval":"1d"}),
            "constants have precedence"
        );
        binding.params = json!({});
        assert_eq!(
            control_metadata(&client, &port, &binding),
            (json!({}), json!({"interval":["5m","1h","1d"]}))
        );
        binding.parameter_fields.clear();
        assert_eq!(
            control_metadata(&client, &port, &binding).1,
            json!({"interval":[]})
        );
        binding.params = json!({"interval":"1w"});
        assert_eq!(
            control_metadata(&client, &port, &binding).0,
            json!({"interval":"1w"}),
            "unsupported fixed intervals stay explicit"
        );
    }
}
