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
    Assistant {
        blocks: Vec<Block>,
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
    Start { model: Model },
    Text { block_id: String, delta: String },
    Thinking { block_id: String, delta: String },
    ToolCall { call: ToolCall },
    ToolUpdate { call_id: String, result: ToolResult },
    ToolResult { call_id: String, result: ToolResult },
    Usage { usage: Usage },
    Error { code: String, message: String },
    Done { reason: DoneReason },
}
