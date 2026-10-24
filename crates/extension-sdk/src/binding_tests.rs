use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

fn object(fields: &[(&str, Schema, bool)]) -> Schema {
    Schema::object(
        fields
            .iter()
            .map(|(name, schema, required)| {
                (
                    (*name).into(),
                    Field {
                        schema: schema.clone(),
                        required: *required,
                        description: String::new(),
                        default: None,
                    },
                )
            })
            .collect(),
    )
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(a, b)| ((*a).into(), (*b).into()))
        .collect()
}

fn array(items: Schema) -> Schema {
    Schema::Array {
        items: Box::new(items),
    }
}

fn nullable(inner: Schema) -> Schema {
    Schema::Nullable {
        inner: Box::new(inner),
    }
}

fn code<T>(result: BackendResult<T>) -> String {
    result.err().expect("expected rejection").code
}

#[test]
fn validation_and_serde_are_structural() {
    let schema = object(&[("n", nullable(Schema::Number), true)]);
    for value in [
        json!({"n":null}),
        json!({"n":4,"extra":true}),
        json!({"n":1.5}),
    ] {
        schema.validate(&value).unwrap();
    }
    for value in [json!({}), json!({"n":"4"}), json!([])] {
        assert!(schema.validate(&value).is_err());
    }
    assert!(Schema::Integer.validate(&json!(1.5)).is_err());
    assert!(Schema::Integer.validate(&json!(1.0)).is_err());
    Schema::Integer.validate(&json!(u64::MAX)).unwrap();
    assert!(array(Schema::String).validate(&json!(["a", 1])).is_err());
    assert!(Schema::Boolean.validate(&json!(1)).is_err());
    assert!(Schema::Null.validate(&json!(false)).is_err());
    let enumeration = Schema::Enum {
        values: vec!["buy".into(), "sell".into()],
    };
    enumeration.validate(&json!("buy")).unwrap();
    assert!(enumeration.validate(&json!("hold")).is_err());
    let tuple = Schema::Tuple {
        items: vec![Schema::String, Schema::Number],
    };
    tuple.validate(&json!(["10", 2])).unwrap();
    assert!(tuple.validate(&json!(["10"])).is_err());
    assert!(tuple.validate(&json!(["10", 2, 3])).is_err());
    let union = Schema::Union {
        variants: vec![Schema::String, Schema::Number],
    };
    union.validate(&json!("10")).unwrap();
    union.validate(&json!(10)).unwrap();
    assert!(union.validate(&json!(false)).is_err());
    let field: Field = serde_json::from_value(json!({"schema":{"type":"number"}})).unwrap();
    assert!(!field.required);
    assert_eq!(field.description, "");
    assert_eq!(field.default, None);
    let value = serde_json::to_value(&schema).unwrap();
    assert_eq!(value["type"], "object");
    assert_eq!(serde_json::from_value::<Schema>(value).unwrap(), schema);
    assert!(Schema::Number.fields().is_none());
}

#[test]
fn compatibility_numeric_nullable_arrays_and_required_fields() {
    let empty = BTreeMap::new();
    assert!(compatible_output(&Schema::Integer, &Schema::Number, &empty));
    assert!(!compatible_output(
        &Schema::Number,
        &Schema::Integer,
        &empty
    ));
    assert!(compatible_output(
        &Schema::Integer,
        &nullable(Schema::Number),
        &empty
    ));
    assert!(compatible_output(
        &Schema::Null,
        &nullable(Schema::Number),
        &empty
    ));
    assert!(!compatible_output(
        &nullable(Schema::Number),
        &Schema::Number,
        &empty
    ));
    assert!(compatible_output(
        &nullable(Schema::Integer),
        &nullable(Schema::Number),
        &empty
    ));
    assert!(!compatible_output(&Schema::Any, &Schema::Number, &empty));
    assert!(compatible_output(&Schema::Number, &Schema::Any, &empty));
    assert!(compatible_output(
        &array(Schema::Integer),
        &array(Schema::Number),
        &empty
    ));
    let required = object(&[("value", Schema::Number, true)]);
    let optional = object(&[("value", Schema::Number, false)]);
    assert!(!compatible_output(&optional, &required, &empty));
    assert!(compatible_output(&required, &optional, &empty));
    assert!(compatible_output(&object(&[]), &optional, &empty));
    assert!(!compatible_output(&object(&[]), &required, &empty));
    let union = Schema::Union {
        variants: vec![Schema::Integer, Schema::String],
    };
    assert!(!compatible_output(&union, &Schema::Number, &empty));
    assert!(compatible_output(&Schema::Integer, &union, &empty));
    assert!(compatible_output(&union, &Schema::Any, &empty));
    let tuple = Schema::Tuple {
        items: vec![Schema::Integer, Schema::Number],
    };
    assert!(compatible_output(&tuple, &array(Schema::Number), &empty));
    assert!(!compatible_output(&array(Schema::Number), &tuple, &empty));
}

#[test]
fn exact_mapping_is_not_potential_renaming() {
    let source = array(object(&[
        ("price", Schema::Number, true),
        ("label", Schema::String, true),
    ]));
    let target = array(object(&[("close", Schema::Number, true)]));
    assert!(!compatible_output(&source, &target, &BTreeMap::new()));
    assert!(potentially_compatible(&source, &target));
    assert!(suggest_output_mapping(&source, &target).is_empty());
    assert!(compatible_output(
        &source,
        &target,
        &map(&[("close", "price")])
    ));
    assert!(!compatible_output(
        &source,
        &target,
        &map(&[("close", "label")])
    ));
    assert!(!compatible_output(
        &source,
        &target,
        &map(&[("close", "typo")])
    ));
    assert!(!compatible_output(
        &source,
        &target,
        &map(&[("typo", "price")])
    ));
    assert_eq!(
        suggest_output_mapping(&source, &source),
        map(&[("price", "price"), ("label", "label")])
    );
    assert!(!potentially_compatible(&Schema::String, &Schema::Number));
    assert!(potentially_compatible(
        &array(Schema::Integer),
        &array(Schema::Number)
    ));
}

struct Typed {
    id: String,
    operation: Operation,
    result: Option<Value>,
    calls: Arc<AtomicUsize>,
}

impl Backend for Typed {
    fn manifest(&self) -> BackendManifest {
        BackendManifest {
            id: self.id.clone(),
            title: self.id.clone(),
            version: "1".into(),
            operations: vec![self.operation.clone()],
        }
    }
    fn execute(&self, _: &str, params: Value) -> BackendResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.result.clone().unwrap_or(params))
    }
}

fn port() -> InputPort {
    InputPort {
        id: "display".into(),
        title: "Display".into(),
        kind: OperationKind::Query,
        parameters: object(&[("symbol", Schema::String, true)]),
        output: object(&[("last", Schema::Number, true)]),
        required: true,
    }
}

fn binding(backend: &str, operation: &str) -> PortBinding {
    PortBinding {
        backend: backend.into(),
        operation: operation.into(),
        params: json!({}),
        fields: map(&[("last", "price")]),
        parameter_fields: BTreeMap::new(),
    }
}

fn poll(request: BoundRequest) -> BackendResult<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match request.try_recv() {
            Ok(result) => return result,
            Err(mpsc::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                std::thread::yield_now()
            }
            other => panic!("request did not complete: {other:?}"),
        }
    }
}

#[test]
fn independent_blocks_map_values_without_binding_side_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let backends: Vec<Arc<dyn Backend>> = [("a", "get_quote", 10), ("b", "fetch_last", 20)]
        .into_iter()
        .map(|(id, operation, price)| {
            Arc::new(Typed {
                id: id.into(),
                operation: Operation::query(
                    operation,
                    operation,
                    object(&[("symbol", Schema::String, true)]),
                    object(&[("price", Schema::Integer, true)]),
                ),
                result: Some(json!({"price":price})),
                calls: calls.clone(),
            }) as Arc<dyn Backend>
        })
        .collect();
    let host = BackendHost::new(backends).unwrap();
    let client = host.client(Permissions::default());
    let first_binding = PortBinding {
        parameter_fields: map(&[("symbol", "symbol")]),
        ..binding("a", "get_quote")
    };
    let second_binding = PortBinding {
        parameter_fields: map(&[("symbol", "symbol")]),
        ..binding("b", "fetch_last")
    };
    let first = BoundInputs::new(
        client.clone(),
        vec![port()],
        BTreeMap::from([("display".into(), first_binding)]),
    )
    .unwrap();
    let second = BoundInputs::new(
        client,
        vec![port()],
        BTreeMap::from([("display".into(), second_binding)]),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let pending = first.submit("display", json!({"symbol":"BTC"})).unwrap();
    drop(first);
    assert_eq!(poll(pending).unwrap(), json!({"last":10}));
    assert_eq!(
        poll(
            second
                .clone()
                .submit("display", json!({"symbol":"BTC"}))
                .unwrap()
        )
        .unwrap(),
        json!({"last":20})
    );
    assert!(second.connected("display"));
    assert!(!second.connected("unknown"));
    assert_eq!(second.bindings()["display"].backend, "b");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn actions_project_dynamic_params_and_constants() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut op = Operation::query(
        "place_order",
        "Place order",
        object(&[
            ("instrument", Schema::String, true),
            ("quantity", Schema::Number, true),
        ]),
        object(&[
            ("instrument", Schema::String, true),
            ("quantity", Schema::Number, true),
        ]),
    );
    op.kind = OperationKind::Action;
    op.access = Access::Trade;
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "broker".into(),
        operation: op,
        result: None,
        calls: calls.clone(),
    })])
    .unwrap();
    let mut port = port();
    port.kind = OperationKind::Action;
    port.parameters = object(&[
        ("symbol", Schema::String, true),
        ("quantity", Schema::Number, false),
    ]);
    port.output = object(&[
        ("instrument", Schema::String, true),
        ("quantity", Schema::Number, true),
    ]);
    let binding = PortBinding {
        backend: "broker".into(),
        operation: "place_order".into(),
        params: json!({"quantity":2}),
        fields: BTreeMap::new(),
        parameter_fields: map(&[("instrument", "symbol")]),
    };
    let public = host.client(Permissions::default());
    assert!(!public.allows(Access::Trade));
    assert_eq!(
        code(validate_binding(&public, &port, &binding)),
        "permission_denied"
    );
    let client = host.client(Permissions {
        trade: true,
        private_read: false,
    });
    let bound = BoundInputs::new(
        client.clone(),
        vec![port.clone()],
        BTreeMap::from([("display".into(), binding.clone())]),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        poll(
            bound
                .submit("display", json!({"symbol":"ETH","quantity":99}))
                .unwrap()
        )
        .unwrap(),
        json!({"instrument":"ETH","quantity":2})
    );
    assert_eq!(code(bound.submit("display", json!({}))), "invalid_params");
    for (to, from) in [("instrument", "typo"), ("typo", "symbol")] {
        let mut bad = binding.clone();
        bad.parameter_fields = map(&[(to, from)]);
        assert_eq!(
            code(validate_binding(&client, &port, &bad)),
            "invalid_binding"
        );
    }
    for value in [json!({"quantity":"2"}), json!({"quantity":2,"typo":1})] {
        let mut bad = binding.clone();
        bad.params = value;
        assert_eq!(
            code(validate_binding(&client, &port, &bad)),
            "invalid_binding"
        );
    }
    let mut bad = binding.clone();
    bad.params = json!({});
    assert_eq!(
        code(validate_binding(&client, &port, &bad)),
        "invalid_binding"
    );
    let mut optional = port.clone();
    if let Schema::Object { fields } = &mut optional.parameters {
        fields.get_mut("symbol").unwrap().required = false;
    }
    assert_eq!(
        code(validate_binding(&client, &optional, &binding)),
        "invalid_binding"
    );
    let mut query = port.clone();
    query.kind = OperationKind::Query;
    assert_eq!(
        code(validate_binding(&client, &query, &binding)),
        "invalid_binding"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn runtime_output_validation_input_protocol_and_unbound_ports() {
    let calls = Arc::new(AtomicUsize::new(0));
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "bad".into(),
        operation: Operation::query(
            "quote",
            "Quote",
            object(&[]),
            object(&[("price", Schema::Number, true)]),
        ),
        result: Some(json!({"price":"not a number"})),
        calls: calls.clone(),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let mut port = port();
    port.parameters = object(&[]);
    let unbound = BoundInputs::new(client.clone(), vec![port.clone()], Bindings::new()).unwrap();
    assert_eq!(code(unbound.submit("display", json!({}))), "unbound_input");
    assert_eq!(code(unbound.submit("unknown", json!({}))), "unknown_input");
    let bound = BoundInputs::new(
        client.clone(),
        vec![port.clone()],
        BTreeMap::from([("display".into(), binding("bad", "quote"))]),
    )
    .unwrap();
    let request: InputRequest = serde_json::from_value(json!({
        "version":2,"id":7,"port":"display","params":{}
    }))
    .unwrap();
    assert_eq!(request.id, 7);
    assert_eq!(
        code(poll(bound.submit_request(request).unwrap())),
        "invalid_output"
    );
    assert_eq!(
        code(bound.submit_request(InputRequest {
            version: 1,
            id: 7,
            port: "display".into(),
            params: json!({})
        })),
        "unsupported_version"
    );
    assert_eq!(
        code(client.call("bad", "quote", json!({}))),
        "invalid_output"
    );
    assert_eq!(
        code(client.submit("bad", "quote", json!(null))),
        "invalid_params"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    for b in [binding("missing", "quote"), binding("bad", "missing")] {
        assert!(validate_binding(&client, &port, &b).is_err());
    }
}

#[test]
fn array_renames_and_any_parameter_constants_work_at_runtime() {
    let source = array(object(&[("price", Schema::Number, true)]));
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "data".into(),
        operation: Operation::query("series", "Series", Schema::Any, source),
        result: Some(json!([{"price": 10}, {"price": 20}])),
        calls: Arc::new(AtomicUsize::new(0)),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let mut port = port();
    port.parameters = Schema::Any;
    port.output = array(object(&[("last", Schema::Number, true)]));
    let mut binding = binding("data", "series");
    binding.params = json!({"arbitrary": true});
    let bound = BoundInputs::new(
        client.clone(),
        vec![port.clone()],
        BTreeMap::from([("display".into(), binding.clone())]),
    )
    .unwrap();
    assert_eq!(
        poll(bound.submit("display", json!({"dynamic": 4})).unwrap()).unwrap(),
        json!([{"last":10}, {"last":20}])
    );
    let mut typo = binding.clone();
    typo.fields.insert("last".into(), "typo".into());
    assert_eq!(
        code(validate_binding(&client, &port, &typo)),
        "invalid_binding"
    );
    assert_eq!(
        code(BoundInputs::new(
            client.clone(),
            vec![port.clone()],
            BTreeMap::from([("typo".into(), binding.clone()),])
        )),
        "invalid_binding"
    );
    assert_eq!(
        code(BoundInputs::new(
            client,
            vec![port.clone(), port],
            BTreeMap::new()
        )),
        "duplicate_input"
    );
}

#[test]
fn any_and_primitive_parameter_forwarding() {
    for schema in [Schema::Any, Schema::Integer] {
        let host = BackendHost::new(vec![Arc::new(Typed {
            id: "echo".into(),
            operation: Operation::query("value", "Value", schema.clone(), Schema::Any),
            result: None,
            calls: Arc::new(AtomicUsize::new(0)),
        })])
        .unwrap();
        let mut port = port();
        port.parameters = schema.clone();
        port.output = Schema::Any;
        let binding = PortBinding {
            backend: "echo".into(),
            operation: "value".into(),
            params: json!({}),
            fields: BTreeMap::new(),
            parameter_fields: BTreeMap::new(),
        };
        let client = host.client(Permissions::default());
        let bound = BoundInputs::new(
            client.clone(),
            vec![port.clone()],
            BTreeMap::from([("display".into(), binding.clone())]),
        )
        .unwrap();
        assert_eq!(
            poll(bound.submit("display", json!(42)).unwrap()).unwrap(),
            json!(42)
        );
        if schema == Schema::Any {
            let binding = PortBinding {
                params: json!({"constant": 2}),
                ..binding
            };
            let bound = BoundInputs::new(
                client,
                vec![port],
                BTreeMap::from([("display".into(), binding)]),
            )
            .unwrap();
            assert_eq!(
                poll(
                    bound
                        .submit("display", json!({"constant": 1,"dynamic": 3}))
                        .unwrap()
                )
                .unwrap(),
                json!({"constant":2,"dynamic":3})
            );
        }
    }
}

#[test]
fn undeclared_request_arguments_never_execute() {
    let calls = Arc::new(AtomicUsize::new(0));
    let nested = object(&[("allowed", Schema::Number, true)]);
    let parameters = object(&[
        ("provider_only", Schema::String, false),
        ("nested", nested.clone(), false),
        ("array", array(nested.clone()), false),
        ("nullable", nullable(nested.clone()), false),
        (
            "union",
            Schema::Union {
                variants: vec![nested.clone(), Schema::Null],
            },
            false,
        ),
        (
            "tuple",
            Schema::Tuple {
                items: vec![nested],
            },
            false,
        ),
    ]);
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "strict".into(),
        operation: Operation::query("echo", "Echo", parameters.clone(), Schema::Any),
        result: None,
        calls: calls.clone(),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let mut frontend = port();
    frontend.parameters = object(&[]);
    frontend.output = Schema::Any;
    let binding = PortBinding {
        backend: "strict".into(),
        operation: "echo".into(),
        params: json!({}),
        fields: BTreeMap::new(),
        parameter_fields: BTreeMap::new(),
    };
    let bound = BoundInputs::new(
        client.clone(),
        vec![frontend.clone()],
        BTreeMap::from([("display".into(), binding.clone())]),
    )
    .unwrap();
    assert_eq!(
        code(bound.submit("display", json!({"provider_only":"injected"}))),
        "invalid_params"
    );
    assert_eq!(
        code(client.submit("strict", "echo", json!({"unknown":1}))),
        "invalid_params"
    );

    frontend.parameters = parameters.clone();
    let dynamic_binding = PortBinding {
        parameter_fields: parameters
            .fields()
            .unwrap()
            .keys()
            .map(|key| (key.clone(), key.clone()))
            .collect(),
        ..binding.clone()
    };
    let bound = BoundInputs::new(
        client.clone(),
        vec![frontend],
        BTreeMap::from([("display".into(), dynamic_binding)]),
    )
    .unwrap();
    for params in [
        json!({"nested":{"allowed":1,"unknown":2}}),
        json!({"array":[{"allowed":1,"unknown":2}]}),
        json!({"nullable":{"allowed":1,"unknown":2}}),
        json!({"union":{"allowed":1,"unknown":2}}),
        json!({"tuple":[{"allowed":1,"unknown":2}]}),
    ] {
        // These same values remain valid output values.
        parameters.validate(&params).unwrap();
        assert_eq!(
            code(client.submit("strict", "echo", params.clone())),
            "invalid_params"
        );
        assert_eq!(code(bound.submit("display", params)), "invalid_params");
    }
    let bad_constants = PortBinding {
        params: json!({"nested":{"allowed":1,"unknown":2}}),
        ..binding
    };
    let mut frontend = port();
    frontend.output = Schema::Any;
    assert_eq!(
        code(validate_binding(&client, &frontend, &bad_constants)),
        "invalid_binding"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        poll(
            bound
                .submit("display", json!({"nested":{"allowed":1}}))
                .unwrap()
        )
        .unwrap(),
        json!({"nested":{"allowed":1}})
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn dynamic_object_parameters_require_explicit_mappings() {
    for required in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let host = BackendHost::new(vec![Arc::new(Typed {
            id: "explicit".into(),
            operation: Operation::query(
                "echo",
                "Echo",
                object(&[("symbol", Schema::String, required)]),
                Schema::Any,
            ),
            result: None,
            calls: calls.clone(),
        })])
        .unwrap();
        let client = host.client(Permissions::default());
        let mut frontend = port();
        frontend.output = Schema::Any;
        let fixed = PortBinding {
            backend: "explicit".into(),
            operation: "echo".into(),
            params: json!({}),
            fields: BTreeMap::new(),
            parameter_fields: BTreeMap::new(),
        };
        if required {
            assert_eq!(
                code(validate_binding(&client, &frontend, &fixed)),
                "invalid_binding"
            );
        } else {
            let bound = BoundInputs::new(
                client.clone(),
                vec![frontend.clone()],
                BTreeMap::from([("display".into(), fixed.clone())]),
            )
            .unwrap();
            // Fixed but blank must omit the argument, not silently forward it.
            assert_eq!(
                poll(
                    bound
                        .submit("display", json!({"symbol":"dynamic"}))
                        .unwrap()
                )
                .unwrap(),
                json!({})
            );
        }
        let mapped = PortBinding {
            parameter_fields: map(&[("symbol", "symbol")]),
            ..fixed.clone()
        };
        let bound = BoundInputs::new(
            client.clone(),
            vec![frontend.clone()],
            BTreeMap::from([("display".into(), mapped)]),
        )
        .unwrap();
        assert_eq!(
            poll(
                bound
                    .submit("display", json!({"symbol":"dynamic"}))
                    .unwrap()
            )
            .unwrap(),
            json!({"symbol":"dynamic"})
        );
        let constant = PortBinding {
            params: json!({"symbol":"constant"}),
            ..fixed
        };
        let bound = BoundInputs::new(
            client,
            vec![frontend],
            BTreeMap::from([("display".into(), constant)]),
        )
        .unwrap();
        assert_eq!(
            poll(
                bound
                    .submit("display", json!({"symbol":"dynamic"}))
                    .unwrap()
            )
            .unwrap(),
            json!({"symbol":"constant"})
        );
        assert_eq!(calls.load(Ordering::SeqCst), if required { 2 } else { 3 });
    }
}

#[test]
fn parameter_compatibility_rejects_extra_nested_fields() {
    let source = object(&[
        ("symbol", Schema::String, true),
        ("venue", Schema::String, false),
    ]);
    let target = object(&[("symbol", Schema::String, true)]);
    assert!(compatible_output(&source, &target, &BTreeMap::new()));
    assert!(!compatible_parameters(&source, &target));
    assert!(compatible_parameters(&target, &source));
    for (source, target) in [
        (array(source.clone()), array(target.clone())),
        (nullable(source.clone()), nullable(target.clone())),
        (
            Schema::Tuple {
                items: vec![source.clone()],
            },
            Schema::Tuple {
                items: vec![target.clone()],
            },
        ),
        (
            Schema::Union {
                variants: vec![source.clone(), Schema::Null],
            },
            Schema::Union {
                variants: vec![target.clone(), Schema::Null],
            },
        ),
    ] {
        assert!(!compatible_parameters(&source, &target));
    }
    assert!(compatible_parameters(&Schema::Integer, &Schema::Number));
    assert!(!compatible_parameters(&Schema::Number, &Schema::Integer));
    assert!(compatible_parameters(&source, &Schema::Any));
    assert!(!compatible_parameters(&Schema::Any, &target));
    assert!(!compatible_parameters(
        &object(&[("symbol", Schema::String, false)]),
        &target
    ));

    let calls = Arc::new(AtomicUsize::new(0));
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "strict".into(),
        operation: Operation::query(
            "echo",
            "Echo",
            object(&[("request", target.clone(), true)]),
            Schema::Any,
        ),
        result: None,
        calls: calls.clone(),
    })])
    .unwrap();
    let client = host.client(Permissions::default());
    let mut frontend = port();
    frontend.parameters = object(&[("request", source, true)]);
    frontend.output = Schema::Any;
    let binding = PortBinding {
        backend: "strict".into(),
        operation: "echo".into(),
        params: json!({}),
        fields: BTreeMap::new(),
        parameter_fields: map(&[("request", "request")]),
    };
    assert_eq!(
        code(validate_binding(&client, &frontend, &binding)),
        "invalid_binding"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    frontend.parameters = object(&[("request", target, true)]);
    let bound = BoundInputs::new(
        client,
        vec![frontend],
        BTreeMap::from([("display".into(), binding)]),
    )
    .unwrap();
    assert_eq!(
        poll(
            bound
                .submit("display", json!({"request":{"symbol":"BTC"}}))
                .unwrap()
        )
        .unwrap(),
        json!({"request":{"symbol":"BTC"}})
    );
}

#[test]
fn output_record_wrappers_map_but_union_renames_are_rejected() {
    let source = object(&[("price", Schema::Number, true)]);
    let target = object(&[("last", Schema::Number, true)]);
    let mapping = map(&[("last", "price")]);
    for wrap in [
        (
            Schema::Union {
                variants: vec![source.clone(), Schema::Null],
            },
            Schema::Union {
                variants: vec![target.clone(), Schema::Null],
            },
        ),
        (
            array(Schema::Union {
                variants: vec![source.clone()],
            }),
            array(Schema::Union {
                variants: vec![target.clone()],
            }),
        ),
        (
            source.clone(),
            Schema::Union {
                variants: vec![target.clone()],
            },
        ),
        (
            Schema::Union {
                variants: vec![source.clone()],
            },
            target.clone(),
        ),
    ] {
        assert!(!compatible_output(&wrap.0, &wrap.1, &mapping));
        assert!(output_fields(&wrap.0).is_none() || output_fields(&wrap.1).is_none());
    }
    let wrapped_source = nullable(array(nullable(array(source))));
    let wrapped_target = nullable(array(nullable(array(target.clone()))));
    assert_eq!(output_fields(&wrapped_target), target.fields());
    assert!(compatible_output(
        &wrapped_source,
        &wrapped_target,
        &mapping
    ));
    let host = BackendHost::new(vec![Arc::new(Typed {
        id: "wrapped".into(),
        operation: Operation::query("series", "Series", object(&[]), wrapped_source),
        result: Some(json!([[{"price":10}], null, []])),
        calls: Arc::new(AtomicUsize::new(0)),
    })])
    .unwrap();
    let mut frontend = port();
    frontend.parameters = object(&[]);
    frontend.output = wrapped_target;
    let bound = BoundInputs::new(
        host.client(Permissions::default()),
        vec![frontend],
        BTreeMap::from([(
            "display".into(),
            PortBinding {
                backend: "wrapped".into(),
                operation: "series".into(),
                params: json!({}),
                fields: mapping,
                parameter_fields: BTreeMap::new(),
            },
        )]),
    )
    .unwrap();
    assert_eq!(
        poll(bound.submit("display", json!({})).unwrap()).unwrap(),
        json!([[{"last":10}], null, []])
    );
}

#[test]
fn standard_schema_values_and_input_registry_validation() {
    candle_schema()
        .validate(&json!([{"time":1700000000,"open":1,"high":2,"low":1,"close":2,"volume":5}]))
        .unwrap();
    assert!(candle_schema().validate(&json!([{"time":1}])).is_err());
    market_schema()
        .validate(&json!([{"symbol":"BTC/USD","base":"BTC","quote":"USD","market":"spot"}]))
        .unwrap();
    ticker_schema()
        .validate(&json!({"symbol":"BTC","last":1,"bid":null,"ask":null,"volume":null}))
        .unwrap();
    assert!(
        ticker_schema()
            .validate(&json!({"symbol":"BTC","last":1}))
            .is_err()
    );
    let mut card = CardManifest {
        id: "card".into(),
        title: "Card".into(),
        renderer: Renderer::Native,
        inputs: vec![port(), port()],
    };
    let manifest = |card| ExtensionManifest {
        id: "ext".into(),
        title: "Ext".into(),
        version: "1".into(),
        cards: vec![card],
    };
    assert_eq!(
        code(ExtensionRegistry::new(vec![manifest(card.clone())])),
        "duplicate_input"
    );
    card.inputs.pop();
    card.inputs[0].id.clear();
    assert_eq!(
        code(ExtensionRegistry::new(vec![manifest(card.clone())])),
        "invalid_manifest"
    );
    card.inputs[0] = port();
    card.inputs[0].title.clear();
    assert_eq!(
        code(ExtensionRegistry::new(vec![manifest(card)])),
        "invalid_manifest"
    );
}
