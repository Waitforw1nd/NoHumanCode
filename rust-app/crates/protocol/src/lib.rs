//! Shared wire types for the Rust host and the future Leptos WASM UI.
//!
//! This crate deliberately has no filesystem, network, database, or UI
//! dependency.  It is the compatibility boundary between the host and its
//! clients.

use serde::{Deserialize, Serialize};

/// Stable error categories exposed by the HTTP/SSE protocol.
///
/// The host may include a human-readable message, but clients should branch
/// on this value rather than parsing Chinese text.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    RequestFailed,
    NotFound,
    Conflict,
    Forbidden,
    Unavailable,
    Internal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

/// The user-visible mode of a session/run.
///
/// The value is part of the protocol.  It must not be inferred from an
/// agent's display name (for example, `chat-session`).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Chat,
    Plan,
    #[default]
    Team,
}

impl SessionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Plan => "plan",
            Self::Team => "team",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_stable_wire_data() {
        assert_eq!(
            serde_json::to_string(&SessionKind::Chat).unwrap(),
            "\"chat\""
        );
        assert_eq!(SessionKind::default(), SessionKind::Team);
        assert_eq!(SessionKind::Plan.as_str(), "plan");
    }

    #[test]
    fn error_wire_shape_is_stable() {
        let body = ErrorBody {
            code: ErrorCode::Unavailable,
            message: "上游暂时不可用".into(),
            retryable: true,
        };
        assert_eq!(
            serde_json::to_string(&body).unwrap(),
            r#"{"code":"unavailable","message":"上游暂时不可用","retryable":true}"#
        );
    }
}
