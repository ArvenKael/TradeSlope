use crate::*;
use std::collections::BTreeMap;

pub const INPUT_PROTOCOL_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPort {
    pub id: String,
    pub title: String,
    pub kind: OperationKind,
    pub parameters: Schema,
    pub output: Schema,
    pub required: bool,
}

pub type Bindings = BTreeMap<String, PortBinding>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub backend: String,
    pub operation: String,
    pub params: Value,
    /// Frontend output field -> backend output field.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// Explicit backend parameter field -> frontend parameter field.
    /// Missing entries never fall back to same-named frontend arguments.
    #[serde(default)]
    pub parameter_fields: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputRequest {
    pub version: u32,
    pub id: u64,
    pub port: String,
    pub params: Value,
}

fn invalid(message: impl Into<String>) -> BackendError {
    BackendError::new("invalid_binding", message)
}

pub(crate) fn validate_ports(ports: &[InputPort]) -> BackendResult<()> {
    let mut ids = HashSet::new();
    for port in ports {
        validate_field(&port.id, "input ID")?;
        validate_field(&port.title, "input title")?;
        if !ids.insert(&port.id) {
            return Err(BackendError::new("duplicate_input", "Duplicate input ID"));
        }
    }
    Ok(())
}

fn operation_for<'a>(
    client: &'a BackendClient,
    binding: &PortBinding,
) -> BackendResult<&'a Operation> {
    let index = client
        .registry
        .indices
        .get(&binding.backend)
        .ok_or_else(|| BackendError::new("unknown_backend", "Unknown backend"))?;
    client.registry.backends[*index]
        .manifest
        .operations
        .iter()
        .find(|op| op.id == binding.operation)
        .ok_or_else(|| BackendError::new("unknown_operation", "Unknown operation"))
}

/// Check metadata only. This never queues work or calls a backend.
pub fn validate_binding(
    client: &BackendClient,
    port: &InputPort,
    binding: &PortBinding,
) -> BackendResult<()> {
    let operation = operation_for(client, binding)?;
    if operation.kind != port.kind {
        return Err(invalid("Operation kind does not match input kind"));
    }
    if !client.allows(operation.access) {
        return Err(BackendError::new("permission_denied", "Permission denied"));
    }
    if !compatible_output(&operation.output, &port.output, &binding.fields) {
        return Err(invalid("Output schemas or field mapping are incompatible"));
    }
    if let Schema::Object { fields } = &operation.parameters {
        let constants = binding
            .params
            .as_object()
            .ok_or_else(|| invalid("Object parameters require object constants"))?;
        for (key, value) in constants {
            let field = fields
                .get(key)
                .ok_or_else(|| invalid(format!("Unknown constant: {key}")))?;
            field
                .schema
                .validate_parameters(value)
                .map_err(|_| invalid(format!("Invalid constant: {key}")))?;
        }
        let frontend = port.parameters.fields();
        for (to, from) in &binding.parameter_fields {
            let target = fields
                .get(to)
                .ok_or_else(|| invalid(format!("Unknown backend argument: {to}")))?;
            let source = frontend
                .and_then(|fields| fields.get(from))
                .ok_or_else(|| invalid(format!("Unknown frontend argument: {from}")))?;
            if !compatible_parameters(&source.schema, &target.schema) {
                return Err(invalid("Incompatible parameter mapping"));
            }
        }
        for (key, target) in fields {
            if constants.contains_key(key) {
                continue;
            }
            let source = binding
                .parameter_fields
                .get(key)
                .and_then(|from| frontend.and_then(|fields| fields.get(from)));
            match source {
                Some(source) => {
                    if (target.required && !source.required)
                        || !compatible_parameters(&source.schema, &target.schema)
                    {
                        return Err(invalid(format!("Incompatible backend argument: {key}")));
                    }
                }
                None if target.required => {
                    return Err(invalid(format!("Missing backend argument: {key}")));
                }
                None => {}
            }
        }
    } else if operation.parameters == Schema::Any {
        // Any has no declared argument keys against which typos can be checked.
        if !binding.parameter_fields.is_empty() {
            return Err(invalid(
                "Parameter mappings require declared backend fields",
            ));
        }
    } else {
        if !binding.parameter_fields.is_empty() {
            return Err(invalid("Parameter mappings require object schemas"));
        }
        if empty_object(&binding.params) {
            if !compatible_parameters(&port.parameters, &operation.parameters) {
                return Err(invalid("Incompatible parameter schemas"));
            }
        } else {
            operation
                .parameters
                .validate_parameters(&binding.params)
                .map_err(|_| invalid("Invalid constant parameters"))?;
        }
    }
    Ok(())
}

fn empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|o| o.is_empty())
}

/// Immutable, independently cloneable input routing for one block instance.
#[derive(Clone)]
pub struct BoundInputs {
    client: BackendClient,
    ports: Vec<InputPort>,
    bindings: Bindings,
}

impl BoundInputs {
    pub fn new(
        client: BackendClient,
        ports: Vec<InputPort>,
        bindings: Bindings,
    ) -> BackendResult<Self> {
        validate_ports(&ports)?;
        for (id, binding) in &bindings {
            let port = ports
                .iter()
                .find(|port| &port.id == id)
                .ok_or_else(|| invalid(format!("Unknown input port: {id}")))?;
            validate_binding(&client, port, binding)?;
        }
        Ok(Self {
            client,
            ports,
            bindings,
        })
    }

    pub fn connected(&self, port: &str) -> bool {
        self.bindings.contains_key(port)
    }

    pub fn bindings(&self) -> &Bindings {
        &self.bindings
    }

    pub fn submit(&self, port: &str, params: Value) -> BackendResult<BoundRequest> {
        let (port, binding, args) = self.prepare(port, params)?;
        let pending = self
            .client
            .submit(&binding.backend, &binding.operation, args)?;
        Ok(BoundRequest {
            pending,
            output: port.output.clone(),
            fields: binding.fields.clone(),
        })
    }

    /// Subscribe only to Query inputs. Actions must remain explicit submissions.
    /// Requests sharing the same mapped backend, operation and arguments share
    /// one host-level poll; output projection remains specific to each input.
    pub fn subscribe(&self, port: &str, params: Value) -> BackendResult<InputSubscription> {
        let (port, binding, args) = self.prepare(port, params)?;
        if port.kind != OperationKind::Query {
            return Err(BackendError::new(
                "not_query",
                "Only Query inputs support subscriptions",
            ));
        }
        self.client.subscribe_input(
            &binding.backend,
            &binding.operation,
            args,
            port.output.clone(),
            binding.fields.clone(),
        )
    }

    fn prepare(
        &self,
        port: &str,
        params: Value,
    ) -> BackendResult<(&InputPort, &PortBinding, Value)> {
        let port = self
            .ports
            .iter()
            .find(|candidate| candidate.id == port)
            .ok_or_else(|| BackendError::new("unknown_input", "Unknown input port"))?;
        let binding = self
            .bindings
            .get(&port.id)
            .ok_or_else(|| BackendError::new("unbound_input", "Input port is not connected"))?;
        port.parameters
            .validate_parameters(&params)
            .map_err(|error| BackendError::new("invalid_params", error.message))?;
        let operation = operation_for(&self.client, binding)?;
        let args = match &operation.parameters {
            Schema::Object { .. } => {
                let mut args = serde_json::Map::new();
                // Only explicitly mapped arguments are projected.
                // Constants win over dynamic values.
                for (key, from) in &binding.parameter_fields {
                    let declared = port
                        .parameters
                        .fields()
                        .is_some_and(|fields| fields.contains_key(from));
                    if let Some(value) = params.get(from).filter(|_| declared) {
                        args.insert(key.clone(), value.clone());
                    }
                }
                args.extend(
                    binding
                        .params
                        .as_object()
                        .expect("validated constants")
                        .clone(),
                );
                Value::Object(args)
            }
            Schema::Any => {
                if let (Some(dynamic), Some(constants)) =
                    (params.as_object(), binding.params.as_object())
                {
                    let mut args = dynamic.clone();
                    args.extend(constants.clone());
                    Value::Object(args)
                } else if empty_object(&binding.params) {
                    params
                } else {
                    binding.params.clone()
                }
            }
            _ if empty_object(&binding.params) => params,
            _ => binding.params.clone(),
        };
        Ok((port, binding, args))
    }

    pub fn submit_request(&self, request: InputRequest) -> BackendResult<BoundRequest> {
        if request.version != INPUT_PROTOCOL_VERSION {
            return Err(BackendError::new(
                "unsupported_version",
                "Unsupported input protocol version",
            ));
        }
        self.submit(&request.port, request.params)
    }
}

pub struct BoundRequest {
    pending: PendingRequest,
    output: Schema,
    fields: BTreeMap<String, String>,
}

impl BoundRequest {
    pub fn try_recv(&self) -> Result<BackendResult<Value>, mpsc::TryRecvError> {
        self.pending
            .try_recv()
            .map(|result| result.and_then(|value| Self::project(value, &self.output, &self.fields)))
    }

    pub(crate) fn project(
        value: Value,
        output: &Schema,
        fields: &BTreeMap<String, String>,
    ) -> BackendResult<Value> {
        let value = map_output(value, output, fields);
        output
            .validate(&value)
            .map_err(|error| BackendError::new("invalid_output", error.message))?;
        Ok(value)
    }
}

fn map_output(value: Value, target: &Schema, mapping: &BTreeMap<String, String>) -> Value {
    match (value, target) {
        (Value::Null, _) => Value::Null,
        (value, Schema::Nullable { inner }) => map_output(value, inner, mapping),
        (Value::Array(values), Schema::Array { items }) => Value::Array(
            values
                .into_iter()
                .map(|v| map_output(v, items, mapping))
                .collect(),
        ),
        (Value::Object(source), Schema::Object { fields }) => Value::Object(
            fields
                .keys()
                .filter_map(|key| {
                    source
                        .get(mapping.get(key).unwrap_or(key))
                        .map(|value| (key.clone(), value.clone()))
                })
                .collect(),
        ),
        (value, _) => value,
    }
}
