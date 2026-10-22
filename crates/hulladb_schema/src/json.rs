use crate::ValidationErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Runtime shapes within one JSON column. Object boundaries are strict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum JsonShape {
    Any,
    String,
    Boolean,
    Number,
    Array {
        item: Box<JsonShape>,
    },
    Object {
        fields: BTreeMap<String, JsonProperty>,
    },
    Nullable {
        inner: Box<JsonShape>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonProperty {
    pub shape: JsonShape,
    pub optional: bool,
}
impl JsonShape {
    pub fn is_bounded(&self) -> bool {
        fn visit(shape: &JsonShape, depth: usize, budget: &mut usize) -> bool {
            if depth > 64 || *budget == 0 {
                return false;
            }
            *budget -= 1;
            match shape {
                JsonShape::Nullable { inner } => visit(inner, depth + 1, budget),
                JsonShape::Array { item } => visit(item, depth + 1, budget),
                JsonShape::Object { fields } => fields
                    .iter()
                    .all(|(name, p)| !name.is_empty() && visit(&p.shape, depth + 1, budget)),
                _ => true,
            }
        }
        visit(self, 0, &mut 100_000)
    }
    /// Validate shape and bound work even for unrestricted JSON.
    pub fn validate(&self, value: &Value) -> Result<(), ValidationErrorKind> {
        let mut budget = 100_000;
        self.visit(value, "$", 0, &mut budget)
    }
    fn visit(
        &self,
        value: &Value,
        path: &str,
        depth: usize,
        budget: &mut usize,
    ) -> Result<(), ValidationErrorKind> {
        let error = |message: &str| ValidationErrorKind::InvalidJson {
            path: path.into(),
            message: message.into(),
        };
        if depth > 64 || *budget == 0 {
            return Err(error("JSON exceeds depth or node limit"));
        }
        *budget -= 1;
        match self {
            Self::Nullable { inner } if !value.is_null() => {
                inner.visit(value, path, depth + 1, budget)
            }
            Self::Nullable { .. } => Ok(()),
            Self::Any => match value {
                Value::Array(values) => {
                    for (i, v) in values.iter().enumerate() {
                        self.visit(v, &format!("{path}[{i}]"), depth + 1, budget)?;
                    }
                    Ok(())
                }
                Value::Object(values) => {
                    for (k, v) in values {
                        self.visit(v, &format!("{path}.{k}"), depth + 1, budget)?;
                    }
                    Ok(())
                }
                _ => Ok(()),
            },
            Self::String if value.is_string() => Ok(()),
            Self::Boolean if value.is_boolean() => Ok(()),
            Self::Number if value.is_number() => Ok(()),
            Self::Array { item } => {
                let values = value.as_array().ok_or_else(|| error("expected array"))?;
                for (i, v) in values.iter().enumerate() {
                    item.visit(v, &format!("{path}[{i}]"), depth + 1, budget)?;
                }
                Ok(())
            }
            Self::Object { fields } => {
                let values = value.as_object().ok_or_else(|| error("expected object"))?;
                for key in values.keys() {
                    if !fields.contains_key(key) {
                        return Err(ValidationErrorKind::InvalidJson {
                            path: format!("{path}.{key}"),
                            message: "unknown property".into(),
                        });
                    }
                }
                for (key, property) in fields {
                    let child = format!("{path}.{key}");
                    if let Some(v) = values.get(key) {
                        property.shape.visit(v, &child, depth + 1, budget)?;
                    } else if !property.optional {
                        return Err(ValidationErrorKind::InvalidJson {
                            path: child,
                            message: "required property missing".into(),
                        });
                    }
                }
                Ok(())
            }
            _ => Err(error("value does not match declared shape")),
        }
    }
    /// Resolve declared object properties, preserving nullable parents for predicates.
    pub fn property(&self, name: &str) -> Option<&JsonShape> {
        match self {
            Self::Nullable { inner } => inner.property(name),
            Self::Object { fields } => fields.get(name).map(|p| &p.shape),
            Self::Any => Some(self),
            _ => None,
        }
    }
    pub fn non_null(&self) -> &Self {
        match self {
            Self::Nullable { inner } => inner.non_null(),
            other => other,
        }
    }
}
