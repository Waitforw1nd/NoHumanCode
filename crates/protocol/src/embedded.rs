//! In-process kernel vocabulary. Storage revisions, model identifiers and tool schemas
//! belong to the embedding host. These types are independent of the SQLite event schema.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub provider: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Content {
    Text { text: String },
    Image { mime_type: String, data: String },
}

/// Archival form of a host message. Only `content` is projected into the model request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: Vec<Content>,
    pub display: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_value"
    )]
    pub details: Option<Value>,
}

// A present JSON null is meaningful in the host's session file.
fn present_value<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SteeringMessage {
    User { content: Vec<Content> },
    Custom(CustomMessage),
}

impl From<SteeringMessage> for Message {
    fn from(input: SteeringMessage) -> Self {
        match input {
            SteeringMessage::User { content } => Self::User { content },
            SteeringMessage::Custom(custom) => Self::Custom(custom),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<Content>,
    pub details: Value,
    pub is_error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Block {
    Text { text: String },
    Image { mime_type: String, data: String },
    Thinking { text: String },
    ToolCall { call: ToolCall },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Message {
    System {
        content: Vec<Content>,
    },
    User {
        content: Vec<Content>,
    },
    Custom(CustomMessage),
    Assistant {
        blocks: Vec<Block>,
    },
    /// An assistant message closed by the user. Partial blocks are archival only.
    Aborted {
        run_id: String,
        partial: Vec<Block>,
    },
    Tool {
        call_id: String,
        name: String,
        result: ToolResult,
    },
    Summary {
        text: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    Message(Message),
    Usage(Usage),
    /// Summarizes the complete selected projection at the expected revision.
    Compaction {
        summary: String,
    },
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub revision: String,
    pub messages: Vec<Message>,
}

/// Per-request limits are checked against the selected provider by `preflight`.
#[derive(Clone, Debug, Default)]
pub struct ModelSettings {
    pub thinking_level: Option<String>,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct RunOptions {
    pub session_id: String,
    pub run_id: String,
    pub model: Model,
    pub system_prompt: String,
    pub content: Vec<Content>,
    pub max_requests: usize,
    pub settings: ModelSettings,
}

#[derive(Clone, Debug)]
pub struct ModelRequest {
    pub session_id: String,
    /// Present on generation requests. Explicit compaction is a session command.
    pub run_id: Option<String>,
    pub settings: ModelSettings,
    pub model: Model,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Clone, Debug)]
pub enum ModelDelta {
    Text { block_id: String, delta: String },
    Thinking { block_id: String, delta: String },
}

#[derive(Clone, Debug)]
pub struct ModelReply {
    /// Complete, successfully finished blocks; a truncated/failed generation is an error.
    pub blocks: Vec<Block>,
    pub usage: Option<Usage>,
}

#[derive(Debug)]
pub struct Invocation {
    pub session_id: String,
    pub run_id: String,
    pub call: ToolCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoneReason {
    Completed,
    Aborted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Start {
        model: Model,
    },
    /// Emitted once per queued input, after its batch was committed to the session.
    InputDelivered {
        message: SteeringMessage,
    },
    Text {
        block_id: String,
        delta: String,
    },
    Thinking {
        block_id: String,
        delta: String,
    },
    ToolCall {
        call: ToolCall,
    },
    ToolUpdate {
        call_id: String,
        result: ToolResult,
    },
    ToolResult {
        call_id: String,
        result: ToolResult,
    },
    Usage {
        usage: Usage,
    },
    Error {
        code: String,
        message: String,
    },
    Done {
        reason: DoneReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_details_round_trip_missing_null_and_structured_json() {
        for details in [
            None,
            Some(Value::Null),
            Some(serde_json::json!({"count": 2})),
        ] {
            let message = CustomMessage {
                custom_type: "report".into(),
                content: vec![Content::Text {
                    text: "done".into(),
                }],
                display: false,
                details,
            };
            let json = serde_json::to_value(&message).unwrap();
            assert_eq!(json.get("details"), message.details.as_ref());
            assert_eq!(
                serde_json::from_value::<CustomMessage>(json).unwrap(),
                message
            );
        }
    }
}
