//! Observations of Provider-reported token usage, supporting reasoning and cached token totals.

use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageObservation {
    #[default]
    Missing,
    Valid(TokenUsage),
    Invalid(UsageInvalidReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageInvalidReason {
    InvalidShape,
    MissingField,
    InvalidInteger,
    OutOfRange,
    InconsistentTotal,
    ConflictingDuplicate,
}

const INVALID_MARKER: &str = "_nhc_usage_invalid";

impl UsageInvalidReason {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidShape => "invalid_shape",
            Self::MissingField => "missing_field",
            Self::InvalidInteger => "invalid_integer",
            Self::OutOfRange => "out_of_range",
            Self::InconsistentTotal => "inconsistent_total",
            Self::ConflictingDuplicate => "conflicting_duplicate",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "invalid_shape" => Self::InvalidShape,
            "missing_field" => Self::MissingField,
            "invalid_integer" => Self::InvalidInteger,
            "out_of_range" => Self::OutOfRange,
            "inconsistent_total" => Self::InconsistentTotal,
            "conflicting_duplicate" => Self::ConflictingDuplicate,
            _ => return None,
        })
    }
}

pub fn classify_usage(value: &Value) -> UsageObservation {
    use UsageInvalidReason as Reason;
    use UsageObservation::{Invalid, Missing, Valid};

    if value.is_null() {
        return Missing;
    }
    let Some(object) = value.as_object() else {
        return Invalid(Reason::InvalidShape);
    };
    if let Some(marker) = object.get(INVALID_MARKER) {
        return Invalid(if object.len() == 1 {
            marker
                .as_str()
                .and_then(Reason::from_code)
                .unwrap_or(Reason::InvalidShape)
        } else {
            Reason::InvalidShape
        });
    }
    let read = |key| -> Result<u64, Reason> {
        let value = object.get(key).ok_or(Reason::MissingField)?;
        let number = value.as_u64().ok_or(Reason::InvalidInteger)?;
        if number > i64::MAX as u64 {
            return Err(Reason::OutOfRange);
        }
        Ok(number)
    };
    let parse = || -> Result<TokenUsage, Reason> {
        let prompt_tokens = read("prompt_tokens")?;
        let completion_tokens = read("completion_tokens")?;
        let raw_total = read("total_tokens")?;
        let sum = prompt_tokens
            .checked_add(completion_tokens)
            .filter(|&s| s <= i64::MAX as u64)
            .ok_or(Reason::InconsistentTotal)?;
        let total_tokens = if raw_total == 0 && sum > 0 {
            sum
        } else {
            raw_total
        };
        if total_tokens < sum {
            return Err(Reason::InconsistentTotal);
        }
        Ok(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
        })
    };
    match parse() {
        Ok(usage) => Valid(usage),
        Err(reason) => Invalid(reason),
    }
}

impl UsageObservation {
    pub(crate) fn observe(&mut self, value: &Value) {
        use UsageObservation::{Invalid, Missing, Valid};
        if matches!(self, Invalid(_)) {
            return;
        }
        *self = match (*self, classify_usage(value)) {
            (previous, Missing) => previous,
            (_, Invalid(reason)) => Invalid(reason),
            (Missing, next) => next,
            (Valid(previous), Valid(next)) if previous == next => Valid(previous),
            (Valid(_), Valid(_)) => Invalid(UsageInvalidReason::ConflictingDuplicate),
            (Invalid(reason), _) => Invalid(reason),
        };
    }

    pub(crate) fn into_value(self) -> Value {
        match self {
            Self::Missing => Value::Null,
            Self::Valid(usage) => json!({
                "prompt_tokens": usage.prompt_tokens,
                "completion_tokens": usage.completion_tokens,
                "total_tokens": usage.total_tokens,
            }),
            Self::Invalid(reason) => json!({INVALID_MARKER: reason.code()}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_reasoning_overhead_and_computes_zero_total() {
        assert_eq!(
            classify_usage(
                &json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 25})
            ),
            UsageObservation::Valid(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 25,
            })
        );
        assert_eq!(
            classify_usage(
                &json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 0})
            ),
            UsageObservation::Valid(TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            })
        );
        assert_eq!(
            classify_usage(
                &json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 14})
            ),
            UsageObservation::Invalid(UsageInvalidReason::InconsistentTotal)
        );
    }
}
