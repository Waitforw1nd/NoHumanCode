//! Shared wire types for the Rust host and the future Leptos WASM UI.
//!
//! This crate deliberately has no filesystem, network, database, or UI
//! dependency.  It is the compatibility boundary between the host and its
//! clients.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Current event envelope written by new persistence code.
pub const EVENT_SCHEMA_VERSION: u32 = 1;

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

/// Lifecycle shared by a session turn and its executable tasks.
///
/// `completed` is only written after the work actually finished.  A crash or
/// a stopped host records `interrupted`, never a forged completion.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Draft,
    AwaitingApproval,
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    AwaitingResume,
    Reviewing,
    Accepted,
    Restored,
    Superseded,
}

impl LifecycleStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::AwaitingResume => "awaiting_resume",
            Self::Reviewing => "reviewing",
            Self::Accepted => "accepted",
            Self::Restored => "restored",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "draft" => Self::Draft,
            "awaiting_approval" => Self::AwaitingApproval,
            "queued" => Self::Queued,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            "interrupted" => Self::Interrupted,
            "awaiting_resume" => Self::AwaitingResume,
            "reviewing" => Self::Reviewing,
            "accepted" => Self::Accepted,
            "restored" => Self::Restored,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }

    pub fn can_transition(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (Self::Draft, Self::AwaitingApproval | Self::Queued)
                | (Self::AwaitingApproval, Self::Queued | Self::Cancelled)
                | (
                    Self::Queued,
                    Self::Running | Self::Failed | Self::Cancelled | Self::Interrupted
                )
                | (
                    Self::Running,
                    Self::Completed | Self::Failed | Self::Cancelled | Self::Interrupted
                )
                | (Self::Interrupted, Self::AwaitingResume)
                | (Self::AwaitingResume, Self::Queued)
                | (
                    Self::Completed,
                    Self::AwaitingResume | Self::Reviewing | Self::Restored | Self::Superseded
                )
                | (
                    Self::Failed,
                    Self::AwaitingResume | Self::Restored | Self::Superseded
                )
                | (
                    Self::Cancelled,
                    Self::AwaitingResume | Self::Restored | Self::Superseded
                )
                | (
                    Self::Reviewing,
                    Self::Accepted | Self::Restored | Self::Superseded
                )
                | (Self::Accepted, Self::Superseded)
                | (Self::Restored, Self::Queued | Self::Superseded)
        )
    }
}

/// Opaque stable identity.  Display names are never used as this value.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ProjectId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct AgentId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TaskId(pub String);

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EventId(pub String);

macro_rules! id_display {
    ($($ty:ty),+ $(,)?) => {$(
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
        impl From<String> for $ty {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl From<&str> for $ty {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
    )+};
}

id_display!(ProjectId, SessionId, TurnId, AgentId, TaskId, EventId);

/// Stable event envelope.  `seq` is the reconnect cursor and is monotonic
/// inside one database.  `cursor` repeats that value as a decimal string so
/// clients do not have to invent their own encoding.
///
/// Legacy rows that predate the envelope still deserialize: missing fields
/// fall back to schema 0 and empty session/turn links.  New writers always
/// fill `schema_version`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventEnvelope {
    #[serde(default)]
    pub schema_version: u32,
    pub seq: i64,
    pub cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub task_id: TaskId,
    pub kind: String,
    pub data: serde_json::Value,
    /// Unix seconds.  The field name is `timestamp`; `at` is accepted when
    /// reading rows written before the envelope existed.
    #[serde(default, alias = "at")]
    pub timestamp: u64,
}

impl<'de> Deserialize<'de> for EventEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            schema_version: u32,
            seq: i64,
            #[serde(default)]
            cursor: String,
            #[serde(default)]
            session_id: Option<SessionId>,
            #[serde(default)]
            turn_id: Option<TurnId>,
            task_id: TaskId,
            kind: String,
            data: serde_json::Value,
            #[serde(default, alias = "at")]
            timestamp: u64,
        }
        let raw = Raw::deserialize(deserializer)?;
        let cursor = if raw.cursor.is_empty() {
            raw.seq.to_string()
        } else {
            raw.cursor
        };
        if cursor != raw.seq.to_string() {
            return Err(serde::de::Error::custom(
                "event cursor must equal the decimal seq",
            ));
        }
        Ok(Self {
            schema_version: raw.schema_version,
            seq: raw.seq,
            cursor,
            session_id: raw.session_id,
            turn_id: raw.turn_id,
            task_id: raw.task_id,
            kind: raw.kind,
            data: raw.data,
            timestamp: raw.timestamp,
        })
    }
}

impl EventEnvelope {
    pub fn legacy(
        seq: i64,
        task_id: String,
        kind: String,
        data: serde_json::Value,
        at: u64,
    ) -> Self {
        Self {
            schema_version: 0,
            seq,
            cursor: seq.to_string(),
            session_id: None,
            turn_id: None,
            task_id: TaskId(task_id),
            kind,
            data,
            timestamp: at,
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

    #[test]
    fn lifecycle_status_round_trips_without_display_names() {
        for status in [
            LifecycleStatus::Queued,
            LifecycleStatus::Running,
            LifecycleStatus::Interrupted,
            LifecycleStatus::Failed,
            LifecycleStatus::Completed,
        ] {
            let wire = serde_json::to_string(&status).unwrap();
            assert_eq!(wire, format!("\"{}\"", status.as_str()));
            assert_eq!(
                serde_json::from_str::<LifecycleStatus>(&wire).unwrap(),
                status
            );
            assert_ne!(status.as_str(), "chat-session");
        }
    }

    #[test]
    fn envelope_keeps_legacy_at_and_fills_cursor() {
        let legacy =
            r#"{"seq":7,"task_id":"task-1","kind":"status","data":{"status":"queued"},"at":11}"#;
        let event: EventEnvelope = serde_json::from_str(legacy).unwrap();
        assert_eq!(event.schema_version, 0);
        assert_eq!(event.seq, 7);
        assert_eq!(event.cursor, "7");
        assert_eq!(event.timestamp, 11);
        assert!(event.session_id.is_none());
        let modern = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            cursor: "7".into(),
            session_id: Some(SessionId("session-1".into())),
            turn_id: Some(TurnId("turn-1".into())),
            ..event
        };
        let encoded = serde_json::to_value(&modern).unwrap();
        assert_eq!(encoded["schema_version"], 1);
        assert_eq!(encoded["cursor"], "7");
        assert_eq!(encoded["session_id"], "session-1");
        assert_eq!(encoded["turn_id"], "turn-1");
        assert_eq!(encoded["timestamp"], 11);
        assert!(encoded.get("at").is_none());
        let forged =
            r#"{"seq":7,"cursor":"8","task_id":"task-1","kind":"status","data":{},"at":11}"#;
        assert!(serde_json::from_str::<EventEnvelope>(forged).is_err());
    }

    #[test]
    fn ids_are_opaque_strings() {
        let project = ProjectId::from("project-1");
        let agent = AgentId::from("agent-1");
        assert_eq!(project.to_string(), "project-1");
        assert_eq!(serde_json::to_string(&agent).unwrap(), "\"agent-1\"");
        assert_ne!(project.0, "chat-session");
    }
}
