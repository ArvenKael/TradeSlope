//! Local structural schemas. No references, coercions, or network resolution.
use crate::{BackendError, BackendResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Schema {
    Any,
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Enum { values: Vec<String> },
    Nullable { inner: Box<Schema> },
    Union { variants: Vec<Schema> },
    Tuple { items: Vec<Schema> },
    Array { items: Box<Schema> },
    Object { fields: BTreeMap<String, Field> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub schema: Schema,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: Option<Value>,
}

impl Schema {
    pub fn object(fields: BTreeMap<String, Field>) -> Self {
        Self::Object { fields }
    }

    pub fn fields(&self) -> Option<&BTreeMap<String, Field>> {
        match self {
            Self::Object { fields } => Some(fields),
            _ => None,
        }
    }

    /// Validate values without coercion. Object schemas allow extra fields.
    /// Defaults are metadata, not implicit values for missing required fields.
    pub fn validate(&self, value: &Value) -> BackendResult<()> {
        self.validate_value(value, false)
    }

    /// Request arguments reject undeclared object keys at every nesting level.
    /// Explicit `Any` schemas remain open to arbitrary values.
    pub(crate) fn validate_parameters(&self, value: &Value) -> BackendResult<()> {
        self.validate_value(value, true)
    }

    fn validate_value(&self, value: &Value, strict: bool) -> BackendResult<()> {
        let valid = match self {
            Self::Any => true,
            Self::Null => value.is_null(),
            Self::Boolean => value.is_boolean(),
            Self::Integer => value.is_i64() || value.is_u64(),
            Self::Number => value.as_f64().is_some_and(f64::is_finite),
            Self::String => value.is_string(),
            Self::Enum { values } => value
                .as_str()
                .is_some_and(|v| values.iter().any(|s| s == v)),
            Self::Nullable { inner } => {
                value.is_null() || inner.validate_value(value, strict).is_ok()
            }
            Self::Union { variants } => variants
                .iter()
                .any(|schema| schema.validate_value(value, strict).is_ok()),
            Self::Tuple { items } => value.as_array().is_some_and(|values| {
                values.len() == items.len()
                    && items
                        .iter()
                        .zip(values)
                        .all(|(schema, value)| schema.validate_value(value, strict).is_ok())
            }),
            Self::Array { items } => value.as_array().is_some_and(|array| {
                array
                    .iter()
                    .all(|v| items.validate_value(v, strict).is_ok())
            }),
            Self::Object { fields } => value.as_object().is_some_and(|object| {
                (!strict || object.keys().all(|key| fields.contains_key(key)))
                    && fields.iter().all(|(key, field)| match object.get(key) {
                        Some(value) => field.schema.validate_value(value, strict).is_ok(),
                        None => !field.required,
                    })
            }),
        };
        if valid {
            Ok(())
        } else {
            Err(BackendError::new(
                "invalid_value",
                "Value does not match schema",
            ))
        }
    }
}

/// Whether all values promised by `source` satisfy `target`, after explicit
/// top-level field renames (also applied to elements of an array of objects).
pub fn compatible_output(
    source: &Schema,
    target: &Schema,
    fields: &BTreeMap<String, String>,
) -> bool {
    if !fields.is_empty() {
        let (Some(source), Some(target)) = (output_fields(source), output_fields(target)) else {
            return false;
        };
        if !fields
            .iter()
            .all(|(to, from)| target.contains_key(to) && source.contains_key(from))
        {
            return false;
        }
    }
    compatible(source, target, fields, Compatibility::Output)
}

/// Whether every value admitted by strict source request validation is also
/// admitted by strict target request validation. No projection or renaming is
/// performed within a parameter value.
pub fn compatible_parameters(source: &Schema, target: &Schema) -> bool {
    compatible(source, target, &BTreeMap::new(), Compatibility::Parameters)
}

#[derive(Clone, Copy)]
enum Compatibility {
    Output,
    PotentialOutput,
    Parameters,
}

fn compatible(
    source: &Schema,
    target: &Schema,
    mapping: &BTreeMap<String, String>,
    mode: Compatibility,
) -> bool {
    use Schema::*;
    match (source, target) {
        (_, Any) => true,
        (Union { variants }, _) => variants
            .iter()
            .all(|source| compatible(source, target, mapping, mode)),
        (_, Union { variants }) => variants
            .iter()
            .any(|target| compatible(source, target, mapping, mode)),
        (Any, _) => false,
        (Nullable { inner: source }, Nullable { inner: target }) => {
            compatible(source, target, mapping, mode)
        }
        (Null, Nullable { .. }) => true,
        (_, Nullable { inner }) => compatible(source, inner, mapping, mode),
        (Nullable { .. }, _) => false,
        (Integer, Number) => true,
        (Enum { .. }, String) => true,
        (Enum { values: source }, Enum { values: target }) => {
            source.iter().all(|v| target.contains(v))
        }
        (Array { items: source }, Array { items: target }) => {
            compatible(source, target, mapping, mode)
        }
        (Tuple { items: source }, Tuple { items: target }) => {
            source.len() == target.len()
                && source
                    .iter()
                    .zip(target)
                    .all(|(source, target)| compatible(source, target, &BTreeMap::new(), mode))
        }
        (Tuple { items }, Array { items: target }) => items
            .iter()
            .all(|source| compatible(source, target, &BTreeMap::new(), mode)),
        (Object { fields: source }, Object { fields: target }) => {
            if matches!(mode, Compatibility::Parameters)
                && source.keys().any(|key| !target.contains_key(key))
            {
                return false;
            }
            target.iter().all(|(key, target)| {
                if matches!(mode, Compatibility::PotentialOutput) {
                    !target.required
                        || source.values().any(|source| {
                            source.required
                                && compatible(
                                    &source.schema,
                                    &target.schema,
                                    &BTreeMap::new(),
                                    mode,
                                )
                        })
                } else {
                    let key = mapping.get(key).unwrap_or(key);
                    match source.get(key) {
                        Some(source) => {
                            (!target.required || source.required)
                                && compatible(
                                    &source.schema,
                                    &target.schema,
                                    &BTreeMap::new(),
                                    mode,
                                )
                        }
                        None => !target.required,
                    }
                }
            })
        }
        _ => source == target,
    }
}

pub fn potentially_compatible(source: &Schema, target: &Schema) -> bool {
    compatible(
        source,
        target,
        &BTreeMap::new(),
        Compatibility::PotentialOutput,
    )
}

/// Record fields beneath any number of nullable/array wrappers.
/// Union and tuple traversal is deliberately unsupported: explicit output
/// field mappings through those contracts are rejected, not guessed.
pub fn output_fields(schema: &Schema) -> Option<&BTreeMap<String, Field>> {
    match schema {
        Schema::Nullable { inner } => output_fields(inner),
        Schema::Array { items } => output_fields(items),
        _ => schema.fields(),
    }
}

/// Suggest exact-name matches only; never guess semantic correspondences.
pub fn suggest_output_mapping(source: &Schema, target: &Schema) -> BTreeMap<String, String> {
    let (Some(source), Some(target)) = (output_fields(source), output_fields(target)) else {
        return BTreeMap::new();
    };
    target
        .iter()
        .filter_map(|(name, target)| {
            source
                .get(name)
                .filter(|source| {
                    (!target.required || source.required)
                        && compatible_output(&source.schema, &target.schema, &BTreeMap::new())
                })
                .map(|_| (name.clone(), name.clone()))
        })
        .collect()
}

fn field(schema: Schema, description: &str) -> Field {
    Field {
        schema,
        required: true,
        description: description.into(),
        default: None,
    }
}

pub fn candle_schema() -> Schema {
    let mut fields = BTreeMap::new();
    fields.insert(
        "time".into(),
        field(Schema::Integer, "Unix timestamp in UTC seconds"),
    );
    for name in ["open", "high", "low", "close", "volume"] {
        fields.insert(name.into(), field(Schema::Number, ""));
    }
    Schema::Array {
        items: Box::new(Schema::object(fields)),
    }
}

pub fn ticker_schema() -> Schema {
    let mut fields = BTreeMap::new();
    fields.insert("symbol".into(), field(Schema::String, ""));
    fields.insert("last".into(), field(Schema::Number, ""));
    for name in ["bid", "ask", "volume"] {
        fields.insert(
            name.into(),
            field(
                Schema::Nullable {
                    inner: Box::new(Schema::Number),
                },
                "",
            ),
        );
    }
    Schema::object(fields)
}

pub fn market_schema() -> Schema {
    Schema::Array {
        items: Box::new(Schema::object(
            ["symbol", "base", "quote", "market"]
                .into_iter()
                .map(|name| (name.into(), field(Schema::String, "")))
                .collect(),
        )),
    }
}
