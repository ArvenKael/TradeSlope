//! Declarative ready datasets, independent of raw operation binding.
use crate::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSource {
    pub id: String,
    pub title: String,
    pub operation: String,
    /// Fixed backend arguments, never UI setup fields.
    pub params: Value,
    /// User setup fields. Field defaults are applied by `source_binding`.
    pub configuration: Schema,
    /// Backend argument -> card-local frontend argument. Optional backend controls
    /// are mapped only when the frontend declares the corresponding argument.
    pub controls: BTreeMap<String, String>,
    /// Optional choices for one configuration argument.
    pub catalog: Option<SourceCatalog>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCatalog {
    pub operation: String,
    pub params: Value,
    pub parameter: String,
    /// Required string fields of each catalog row.
    pub value_field: String,
    pub label_field: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub backend: String,
    pub backend_title: String,
    pub source: DataSource,
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::new("invalid_source", message)
}

fn query<'a>(manifest: &'a BackendManifest, id: &str) -> BackendResult<&'a Operation> {
    let op = manifest
        .operations
        .iter()
        .find(|op| op.id == id)
        .ok_or_else(|| invalid(format!("Unknown source operation: {id}")))?;
    if op.kind != OperationKind::Query || op.access != Access::Public {
        return Err(invalid(
            "Sources and catalogs require public Query operations",
        ));
    }
    Ok(op)
}

pub(crate) fn validate_sources(
    manifest: &BackendManifest,
    sources: &[DataSource],
) -> BackendResult<()> {
    let mut ids = HashSet::new();
    for source in sources {
        if source.id.trim().is_empty() || source.title.trim().is_empty() || !ids.insert(&source.id)
        {
            return Err(invalid(
                "Empty or duplicate source ID, or empty source title",
            ));
        }
        let op = query(manifest, &source.operation)?;
        let parameters = op
            .parameters
            .fields()
            .ok_or_else(|| invalid("Sources require object parameters"))?;
        let configuration = source
            .configuration
            .fields()
            .ok_or_else(|| invalid("Source configuration must be an object"))?;
        let constants = source
            .params
            .as_object()
            .ok_or_else(|| invalid("Source params must be an object"))?;
        for (key, value) in constants {
            let field = parameters
                .get(key)
                .ok_or_else(|| invalid(format!("Unknown source constant: {key}")))?;
            field
                .schema
                .validate_parameters(value)
                .map_err(|_| invalid(format!("Invalid source constant: {key}")))?;
        }
        for (key, field) in configuration {
            let target = parameters
                .get(key)
                .ok_or_else(|| invalid(format!("Unknown configuration field: {key}")))?;
            if constants.contains_key(key)
                || source.controls.contains_key(key)
                || !compatible_parameters(&field.schema, &target.schema)
                || (target.required && !field.required)
            {
                return Err(invalid(format!("Incompatible configuration field: {key}")));
            }
            if let Some(default) = &field.default {
                field
                    .schema
                    .validate_parameters(default)
                    .map_err(|_| invalid(format!("Invalid configuration default: {key}")))?;
            }
        }
        for (key, from) in &source.controls {
            if !parameters.contains_key(key) || from.trim().is_empty() {
                return Err(invalid(format!("Invalid source control: {key}")));
            }
        }
        for (key, field) in parameters {
            if field.required
                && !constants.contains_key(key)
                && !configuration.contains_key(key)
                && !source.controls.contains_key(key)
            {
                return Err(invalid(format!("Missing source argument: {key}")));
            }
        }
        if let Some(catalog) = &source.catalog {
            let op = query(manifest, &catalog.operation)?;
            op.parameters
                .validate_parameters(&catalog.params)
                .map_err(|_| invalid("Invalid catalog params"))?;
            let Schema::Array { items } = &op.output else {
                return Err(invalid("Catalog output must be an array of objects"));
            };
            let fields = items
                .fields()
                .ok_or_else(|| invalid("Catalog output must be an array of objects"))?;
            for key in [&catalog.value_field, &catalog.label_field] {
                if !fields.get(key).is_some_and(|field| {
                    field.required && compatible_parameters(&field.schema, &Schema::String)
                }) {
                    return Err(invalid("Catalog fields must be required strings"));
                }
            }
            let symbol = configuration
                .get(&catalog.parameter)
                .ok_or_else(|| invalid("Catalog requires a declared configuration parameter"))?;
            if !compatible_parameters(&Schema::String, &symbol.schema) {
                return Err(invalid("Catalog symbol configuration must accept strings"));
            }
        }
    }
    Ok(())
}

/// Build a typed binding using registration metadata only; never performs I/O.
/// Only declared setup fields are accepted. Missing fields use declared defaults.
/// Declared frontend controls are dynamic: their fixed defaults are removed so
/// they cannot shadow the card's current value. Unmapped constants remain intact;
/// missing required backend arguments are still rejected by binding validation.
/// Output fields match exact names only; semantic renaming belongs to advanced binding.
pub fn source_binding(
    client: &BackendClient,
    port: &InputPort,
    source_backend: &str,
    source_id: &str,
    configuration: Value,
) -> BackendResult<PortBinding> {
    let index = client
        .registry
        .indices
        .get(source_backend)
        .ok_or_else(|| BackendError::new("unknown_backend", "Unknown backend"))?;
    let backend = &client.registry.backends[*index];
    let source = backend
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .ok_or_else(|| BackendError::new("unknown_source", "Unknown source"))?;
    let mut configuration = configuration
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Configuration must be an object"))?;
    for (key, field) in source
        .configuration
        .fields()
        .expect("validated configuration")
    {
        if !configuration.contains_key(key)
            && let Some(default) = &field.default
        {
            configuration.insert(key.clone(), default.clone());
        }
    }
    source
        .configuration
        .validate_parameters(&Value::Object(configuration.clone()))
        .map_err(|_| invalid("Configuration does not match source schema"))?;
    let mut params = source.params.as_object().expect("validated params").clone();
    params.extend(configuration);
    let parameter_fields: BTreeMap<_, _> = source
        .controls
        .iter()
        .filter(|(_, from)| {
            port.parameters
                .fields()
                .is_some_and(|fields| fields.contains_key(*from))
        })
        .map(|(to, from)| (to.clone(), from.clone()))
        .collect();
    for key in parameter_fields.keys() {
        params.remove(key);
    }
    let op = query(&backend.manifest, &source.operation)?;
    let fields = output_fields(&port.output)
        .map(|fields| {
            fields
                .keys()
                .filter(|key| {
                    output_fields(&op.output).is_some_and(|source| source.contains_key(*key))
                })
                .map(|key| (key.clone(), key.clone()))
                .collect()
        })
        .unwrap_or_default();
    if !compatible_output(&op.output, &port.output, &fields) {
        return Err(invalid("Source output is incompatible with input"));
    }
    let binding = PortBinding {
        backend: source_backend.into(),
        operation: source.operation.clone(),
        params: Value::Object(params),
        fields,
        parameter_fields,
    };
    validate_binding(client, port, &binding)?;
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn field(schema: Schema) -> Field {
        Field {
            schema,
            required: true,
            description: String::new(),
            default: None,
        }
    }

    fn descriptor() -> DataSource {
        DataSource {
            id: "candles".into(),
            title: "Spot".into(),
            operation: "candles".into(),
            params: json!({"market":"spot","interval":"1m"}),
            configuration: Schema::object(
                [(
                    "symbol".into(),
                    Field {
                        default: Some(json!("BTC_USDT")),
                        ..field(Schema::String)
                    },
                )]
                .into(),
            ),
            controls: [("interval".into(), "interval".into())].into(),
            catalog: Some(SourceCatalog {
                operation: "markets".into(),
                params: json!({}),
                parameter: "symbol".into(),
                value_field: "symbol".into(),
                label_field: "symbol".into(),
            }),
        }
    }

    struct Provider(Vec<DataSource>);
    impl Backend for Provider {
        fn manifest(&self) -> BackendManifest {
            BackendManifest {
                id: "test".into(),
                title: "Test".into(),
                version: "1".into(),
                operations: vec![
                    Operation::query(
                        "candles",
                        "Candles",
                        Schema::object(
                            [
                                ("market".into(), field(Schema::String)),
                                ("symbol".into(), field(Schema::String)),
                                (
                                    "interval".into(),
                                    field(Schema::Enum {
                                        values: vec!["1m".into(), "1h".into()],
                                    }),
                                ),
                                (
                                    "end".into(),
                                    Field {
                                        required: false,
                                        ..field(Schema::Integer)
                                    },
                                ),
                                (
                                    "limit".into(),
                                    Field {
                                        required: false,
                                        ..field(Schema::Integer)
                                    },
                                ),
                            ]
                            .into(),
                        ),
                        Schema::Any,
                    ),
                    Operation::query(
                        "markets",
                        "Markets",
                        Schema::object(BTreeMap::new()),
                        market_schema(),
                    ),
                ],
            }
        }
        fn sources(&self) -> Vec<DataSource> {
            self.0.clone()
        }
        fn execute(&self, _: &str, params: Value) -> BackendResult<Value> {
            Ok(params)
        }
    }

    fn port() -> InputPort {
        InputPort {
            id: "data".into(),
            title: "Data".into(),
            kind: OperationKind::Query,
            parameters: Schema::object(
                [(
                    "interval".into(),
                    field(Schema::Enum {
                        values: vec!["1h".into()],
                    }),
                )]
                .into(),
            ),
            output: Schema::Any,
            required: true,
        }
    }

    #[test]
    fn controls_are_dynamic_defaults_and_setup_are_strict() {
        let host = BackendHost::new(vec![Arc::new(Provider(vec![descriptor()]))]).unwrap();
        let client = host.client(Permissions::default());
        assert_eq!(client.sources()[0].backend_title, "Test");
        let binding = source_binding(&client, &port(), "test", "candles", json!({})).unwrap();
        assert_eq!(binding.params, json!({"market":"spot","symbol":"BTC_USDT"}));
        let inputs = BoundInputs::new(
            client.clone(),
            vec![port()],
            [("data".into(), binding)].into(),
        )
        .unwrap();
        let request = inputs.submit("data", json!({"interval":"1h"})).unwrap();
        loop {
            match request.try_recv() {
                Ok(value) => {
                    assert_eq!(value.unwrap()["interval"], "1h");
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => std::thread::yield_now(),
                Err(error) => panic!("{error}"),
            }
        }
        for config in [
            json!({"interval":"1m"}),
            json!({"symbol":3}),
            json!({"market":"futures"}),
            json!(null),
        ] {
            assert!(source_binding(&client, &port(), "test", "candles", config).is_err());
        }
        let mut unsupported = port();
        unsupported.parameters = Schema::object(
            [(
                "interval".into(),
                field(Schema::Enum {
                    values: vec!["2m".into()],
                }),
            )]
            .into(),
        );
        assert!(source_binding(&client, &unsupported, "test", "candles", json!({})).is_err());
        unsupported.parameters =
            Schema::object([("timeframe".into(), field(Schema::String))].into());
        let fallback = source_binding(&client, &unsupported, "test", "candles", json!({})).unwrap();
        assert_eq!(fallback.params["interval"], "1m");
        assert!(fallback.parameter_fields.is_empty());
        let mut missing = descriptor();
        missing.params.as_object_mut().unwrap().remove("interval");
        let host = BackendHost::new(vec![Arc::new(Provider(vec![missing]))]).unwrap();
        assert!(
            source_binding(
                &host.client(Permissions::default()),
                &unsupported,
                "test",
                "candles",
                json!({})
            )
            .is_err()
        );
    }

    #[test]
    fn optional_controls_follow_frontend_declarations_and_preserve_unmapped_defaults() {
        let mut source = descriptor();
        source.params["limit"] = json!(200);
        source.controls.extend([
            ("end".into(), "end".into()),
            ("limit".into(), "limit".into()),
        ]);
        let host = BackendHost::new(vec![Arc::new(Provider(vec![source]))]).unwrap();
        let client = host.client(Permissions::default());
        let old = source_binding(&client, &port(), "test", "candles", json!({})).unwrap();
        assert_eq!(
            old.parameter_fields,
            [("interval".into(), "interval".into())].into()
        );
        assert_eq!(old.params["limit"], 200);
        let mut history = port();
        let Schema::Object { fields } = &mut history.parameters else {
            unreachable!()
        };
        for key in ["end", "limit"] {
            fields.insert(
                key.into(),
                Field {
                    required: false,
                    ..field(Schema::Integer)
                },
            );
        }
        let binding = source_binding(&client, &history, "test", "candles", json!({})).unwrap();
        assert_eq!(binding.parameter_fields.len(), 3);
        assert!(binding.params.get("limit").is_none());
        let inputs =
            BoundInputs::new(client, vec![history], [("data".into(), binding)].into()).unwrap();
        let request = inputs
            .submit(
                "data",
                json!({"interval":"1h","end":1700000000,"limit":1000}),
            )
            .unwrap();
        loop {
            match request.try_recv() {
                Ok(result) => {
                    assert_eq!(
                        result.unwrap(),
                        json!({
                            "market":"spot","symbol":"BTC_USDT","interval":"1h",
                            "end":1700000000,"limit":1000
                        })
                    );
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => std::thread::yield_now(),
                Err(error) => panic!("{error}"),
            }
        }
        assert!(
            inputs
                .submit("data", json!({"interval":"1h","unknown":1}))
                .is_err()
        );
    }

    #[test]
    fn invalid_registration_is_rejected_but_raw_operations_remain_available() {
        let source = descriptor();
        let mut cases = vec![vec![source.clone(), source.clone()]];
        for change in 0..7 {
            let mut source = source.clone();
            match change {
                0 => source.operation = "unknown".into(),
                1 => source.params["typo"] = json!(true),
                2 => {
                    source.controls.insert("typo".into(), "interval".into());
                }
                3 => source.catalog.as_mut().unwrap().operation = "unknown".into(),
                4 => source.catalog.as_mut().unwrap().params = json!({"typo":true}),
                5 => source.catalog.as_mut().unwrap().value_field = "typo".into(),
                _ => source.catalog.as_mut().unwrap().parameter = "typo".into(),
            }
            cases.push(vec![source]);
        }
        for sources in cases {
            assert_eq!(
                BackendHost::new(vec![Arc::new(Provider(sources))])
                    .err()
                    .unwrap()
                    .code,
                "invalid_source"
            );
        }
        let host = BackendHost::new(vec![Arc::new(Provider(vec![]))]).unwrap();
        let client = host.client(Permissions::default());
        assert!(client.sources().is_empty());
        assert_eq!(
            client.call("test", "candles", json!({})).unwrap_err().code,
            "invalid_params"
        );
        assert_eq!(
            client.call("test", "absent", json!({})).unwrap_err().code,
            "unknown_operation"
        );
    }

    #[test]
    fn runtime_wrapper_forwards_sources_and_readonly_catalog_is_required() {
        let runtime = runtime::BackendRuntime::new(
            runtime::Database::in_memory().unwrap(),
            vec![Arc::new(Provider(vec![descriptor()]))],
        )
        .unwrap();
        let host = BackendHost::new(runtime.backends()).unwrap();
        assert_eq!(
            host.client(Permissions::default()).sources()[0].source,
            descriptor()
        );
        let mut manifest = Provider(vec![]).manifest();
        manifest.operations[1].access = Access::PrivateRead;
        assert!(validate_sources(&manifest, &[descriptor()]).is_err());
        manifest.operations[1].access = Access::Public;
        manifest.operations[1].kind = OperationKind::Action;
        assert!(validate_sources(&manifest, &[descriptor()]).is_err());
    }
}
