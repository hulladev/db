use crate::{FieldDefinition, ScalarType, Value};

/// Portable constraints in the field's stored representation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldConstraints {
    pub min: Option<Value>,
    pub max: Option<Value>,
    pub one_of: Vec<Value>,
}

impl FieldConstraints {
    pub(crate) fn validate_definition(&self, field: &FieldDefinition) -> bool {
        let bound_valid = |v: &Value| match (field.scalar_type, v) {
            (ScalarType::String, Value::Int64(n)) => *n >= 0,
            (ScalarType::Int64, Value::Int64(_)) => true,
            (ScalarType::Float64, Value::Float64(n)) => n.as_f64().is_some_and(f64::is_finite),
            _ => false,
        };
        if self
            .min
            .iter()
            .chain(self.max.iter())
            .any(|v| !bound_valid(v))
        {
            return false;
        }
        if let (Some(a), Some(b)) = (&self.min, &self.max) {
            let ordered = match (a, b) {
                (Value::Int64(a), Value::Int64(b)) => a <= b,
                (Value::Float64(a), Value::Float64(b)) => a.as_f64() <= b.as_f64(),
                _ => false,
            };
            if !ordered {
                return false;
            }
        }
        let mut base = field.clone();
        base.constraints = Self::default();
        for (i, v) in self.one_of.iter().enumerate() {
            if matches!(v, Value::Null | Value::Json(_))
                || base.validate_value(v).is_err()
                || !self.within_bounds(v)
                || self.one_of[..i]
                    .iter()
                    .any(|previous| same_value(previous, v))
            {
                return false;
            }
        }
        true
    }

    fn within_bounds(&self, value: &Value) -> bool {
        let compare = |bound: &Value, lower: bool| match (value, bound) {
            (Value::String(v), Value::Int64(n)) => {
                if lower {
                    v.chars().count() as u128 >= *n as u128
                } else {
                    v.chars().count() as u128 <= *n as u128
                }
            }
            (Value::Int64(v), Value::Int64(n)) => {
                if lower {
                    v >= n
                } else {
                    v <= n
                }
            }
            (Value::Float64(v), Value::Float64(n)) => {
                if lower {
                    v.as_f64() >= n.as_f64()
                } else {
                    v.as_f64() <= n.as_f64()
                }
            }
            _ => false,
        };
        self.min.as_ref().is_none_or(|b| compare(b, true))
            && self.max.as_ref().is_none_or(|b| compare(b, false))
    }

    pub(crate) fn accepts(&self, value: &Value) -> bool {
        matches!(value, Value::Null)
            || (self.within_bounds(value)
                && (self.one_of.is_empty()
                    || self.one_of.iter().any(|choice| same_value(choice, value))))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDeclaration {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOperand {
    Field(String),
    Value(Value),
}

/// One trusted SQL expression with explicitly quoted field and literal operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDefinition {
    pub name: String,
    pub parts: Vec<String>,
    pub operands: Vec<CheckOperand>,
}

fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float64(left), Value::Float64(right)) => left.as_f64() == right.as_f64(),
        (Value::Timestamp(left), Value::Timestamp(right)) => {
            crate::temporal::timestamp(left, 6, true)
                .zip(crate::temporal::timestamp(right, 6, true))
                .is_some_and(|(left, right)| left == right)
        }
        (Value::LocalTimestamp(left), Value::LocalTimestamp(right)) => {
            crate::temporal::timestamp(left, 6, false)
                .zip(crate::temporal::timestamp(right, 6, false))
                .is_some_and(|(left, right)| left == right)
        }
        _ => left == right,
    }
}
