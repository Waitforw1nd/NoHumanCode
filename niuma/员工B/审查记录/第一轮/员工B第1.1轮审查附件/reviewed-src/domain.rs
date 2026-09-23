use anyhow::{Result, bail, ensure};
pub use peachsh_protocol::{
    AgentId, EVENT_SCHEMA_VERSION, EventEnvelope, EventId, LifecycleStatus, ProjectId, SessionId,
    SessionKind, TaskId, TurnId,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest;
use std::{
    collections::HashSet,
    path::{Component, Path},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn concurrency() -> usize {
    3
}
fn tokens() -> u32 {
    4096
}
fn rounds() -> usize {
    12
}
fn one() -> usize {
    1
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Route {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    #[serde(default = "tokens")]
    pub max_tokens: u32,
    #[serde(default = "one")]
    pub parallel_limit: usize,
    #[serde(default)]
    pub key_env: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Account {
    pub base_url: String,
    pub user_id: String,
    #[serde(default = "quota_unit")]
    pub quota_per_unit: f64,
}
fn quota_unit() -> f64 {
    500_000.0
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Settings {
    pub workspace: String,
    #[serde(default = "concurrency")]
    pub max_concurrency: usize,
    #[serde(default)]
    pub routes: Vec<Route>,
    pub newapi: Option<Account>,
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=8).contains(&self.max_concurrency),
            "并发数必须在 1–8 之间"
        );
        ensure!(Path::new(&self.workspace).is_dir(), "工作目录不存在");
        let mut ids = HashSet::new();
        for route in &self.routes {
            ensure!(
                valid_name(&route.id) && ids.insert(&route.id),
                "路由 ID 无效或重复"
            );
            ensure!(
                route.id != "newapi-account",
                "路由 ID 保留给 New API 账号，不能用于模型路由"
            );
            ensure!(
                !route.name.trim().is_empty() && !route.model.trim().is_empty(),
                "路由名称和模型不能为空"
            );
            validate_url(&route.base_url)?;
            ensure!(
                (1..=8).contains(&route.parallel_limit),
                "每个 Key 的并发必须在 1–8 之间"
            );
            ensure!(
                (16..=65536).contains(&route.max_tokens),
                "输出上限必须在 16–65536 之间"
            );
        }
        if let Some(account) = &self.newapi {
            validate_url(&account.base_url)?;
            ensure!(
                account.user_id.parse::<u64>().is_ok(),
                "New API 用户 ID 必须是数字"
            );
            ensure!(
                account.quota_per_unit.is_finite() && account.quota_per_unit > 0.0,
                "额度换算比例必须大于零"
            );
        }
        Ok(())
    }
}

pub fn validate_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        matches!(url.scheme(), "https" | "http") && url.host_str().is_some(),
        "API 地址必须是 HTTP 或 HTTPS"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "API 地址不能包含凭据、查询或片段"
    );
    Ok(())
}
pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn scope_path(value: &str) -> Result<String> {
    if value == "*" {
        return Ok("*".into());
    }
    let value = value.replace('\\', "/");
    ensure!(
        !value.is_empty() && value != "." && !value.contains(':') && !value.starts_with('/'),
        "范围必须是项目内的相对路径"
    );
    ensure!(
        Path::new(&value)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "路径不允许包含 .. 或根目录"
    );
    ensure!(
        !value
            .split('/')
            .any(|s| s.is_empty() || s.ends_with('.') || s.ends_with(' ')),
        "无效的路径段"
    );
    ensure!(
        !value
            .chars()
            .any(|c| c.is_control() || "<>\"|?*".contains(c)),
        "路径包含不允许的字符"
    );
    ensure!(
        !value.split('/').any(|part| {
            let stem = part
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase();
            matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit())
        }),
        "不允许使用 Windows 设备名称"
    );
    Ok(value.trim_end_matches('/').to_lowercase())
}
pub fn overlaps(a: &str, b: &str) -> bool {
    a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaskSpec {
    pub name: String,
    pub role: String,
    pub route_id: String,
    pub prompt: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub write_scopes: Vec<String>,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub allow_commands: bool,
    #[serde(default = "rounds")]
    pub max_rounds: usize,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub title: String,
    #[serde(default)]
    pub kind: SessionKind,
    pub tasks: Vec<TaskSpec>,
}
impl RunRequest {
    pub fn validate(&self, settings: &Settings) -> Result<()> {
        ensure!(
            !self.title.trim().is_empty() && self.title.len() <= 500,
            "请填写任务标题（不超过 500 字节）"
        );
        ensure!(
            !self.tasks.is_empty() && self.tasks.len() <= 32,
            "每批需要 1–32 个任务"
        );
        if self.kind == SessionKind::Chat {
            ensure!(
                self.tasks.len() == 1 && self.tasks[0].depends_on.is_empty(),
                "对话模式只能包含一个没有前置依赖的任务"
            );
        }
        ensure!(
            self.kind != SessionKind::Plan,
            "计划模式尚未进入派工阶段，请先确认计划后再启动团队任务"
        );
        let mut names = HashSet::new();
        let all_names: HashSet<&str> = self.tasks.iter().map(|t| t.name.as_str()).collect();
        for task in &self.tasks {
            ensure!(
                task.depends_on
                    .iter()
                    .all(|n| n != &task.name && all_names.contains(n.as_str())),
                "前置成员不存在或引用了自己"
            );
        }
        let mut resolved = HashSet::new();
        loop {
            let before = resolved.len();
            for task in &self.tasks {
                if task
                    .depends_on
                    .iter()
                    .all(|n| resolved.contains(n.as_str()))
                {
                    resolved.insert(task.name.as_str());
                }
            }
            if resolved.len() == all_names.len() {
                break;
            }
            ensure!(resolved.len() > before, "成员之间存在循环依赖");
        }
        let mut scopes: Vec<(String, String)> = vec![];
        for task in &self.tasks {
            ensure!(
                valid_name(&task.name) && names.insert(&task.name),
                "成员名称必须唯一，使用小写英文、数字和连字符"
            );
            ensure!(
                !task.role.trim().is_empty() && task.role.len() <= 200,
                "职责不能为空或过长"
            );
            ensure!(
                !task.prompt.trim().is_empty() && task.prompt.len() <= 100_000,
                "任务内容为空或过长"
            );
            ensure!(
                (1..=30).contains(&task.max_rounds),
                "工具轮数必须在 1–30 之间"
            );
            ensure!(
                settings.routes.iter().any(|r| r.id == task.route_id),
                "未知的模型路由：{}",
                task.route_id
            );
            for scope in &task.write_scopes {
                let normalized = scope_path(scope)?;
                for (owner, previous) in &scopes {
                    if owner != &task.name
                        && overlaps(&normalized, previous)
                        && !self.depends_on(&task.name, owner)
                        && !self.depends_on(owner, &task.name)
                    {
                        bail!("写入范围冲突：{} 与 {}", owner, task.name);
                    }
                }
                scopes.push((task.name.clone(), normalized));
            }
        }
        Ok(())
    }
    fn depends_on(&self, name: &str, ancestor: &str) -> bool {
        let mut pending = vec![name];
        let mut visited = HashSet::new();
        while let Some(name) = pending.pop() {
            if !visited.insert(name) {
                continue;
            }
            if let Some(task) = self.tasks.iter().find(|t| t.name == name) {
                for dependency in &task.depends_on {
                    if dependency == ancestor {
                        return true;
                    }
                    pending.push(dependency);
                }
            }
        }
        false
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub run_id: String,
    pub spec: TaskSpec,
    pub route: Route,
    pub workspace: String,
    pub status: String,
    pub output: String,
    pub error: Option<String>,
    pub messages: Vec<Value>,
    pub usage: Value,
    pub created_at: u64,
    pub updated_at: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: SessionKind,
    pub created_at: u64,
    pub tasks: Vec<Task>,
}

/// Compatibility projection of [`EventEnvelope`].
///
/// Existing HTTP/SSE clients read `seq`, `task_id`, `kind`, `data`, and `at`.
/// New persistence also stores `schema_version`, `cursor`, `session_id`, and
/// `turn_id`.  Those extra fields are optional on read so schema 5 rows still
/// decode.  `at` remains the timestamp alias and is always written.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    #[serde(default)]
    pub schema_version: u32,
    pub seq: i64,
    #[serde(default)]
    pub cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub task_id: String,
    pub kind: String,
    pub data: Value,
    #[serde(alias = "timestamp")]
    pub at: u64,
}

impl Event {
    pub fn envelope(&self) -> EventEnvelope {
        EventEnvelope {
            schema_version: self.schema_version,
            seq: self.seq,
            cursor: if self.cursor.is_empty() {
                self.seq.to_string()
            } else {
                self.cursor.clone()
            },
            session_id: self.session_id.clone().map(SessionId),
            turn_id: self.turn_id.clone().map(TurnId),
            task_id: TaskId(self.task_id.clone()),
            kind: self.kind.clone(),
            data: self.data.clone(),
            timestamp: self.at,
        }
    }
}

/// User-selected local directory.  Identity is the project id, not the path
/// text and not any agent display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub root_path: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Continuous user-visible context bound to exactly one project.
///
/// `kind` is stored from the protocol field.  Callers must not derive it from
/// `title` or from a member display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub project_id: ProjectId,
    pub kind: SessionKind,
    pub title: String,
    /// Legacy `runs.id` while the old HTTP surface still addresses runs.
    /// This is a stored mapping, not an alias of `SessionId`. Current creation
    /// assigns an independent run id; callers must read this field.
    pub legacy_run_id: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// One user request inside a session, and the recovery unit for that request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub session_id: SessionId,
    pub project_id: ProjectId,
    pub status: LifecycleStatus,
    pub request_hash: String,
    /// Stable idempotency key when the caller supplied one.  Never a secret.
    pub idempotency_key: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Stable agent identity.  `display_name` may change; `id` must not.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub session_id: SessionId,
    pub display_name: String,
    pub role: String,
    pub created_at: u64,
}

/// Executable unit inside a turn.  Dependencies, when present, use task ids
/// rather than display names.  `legacy_task_id` keeps the old `tasks` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnTask {
    pub id: TaskId,
    pub turn_id: TurnId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub legacy_task_id: String,
    /// Stable task ids in the same turn.  Display names are not dependencies.
    #[serde(default)]
    pub depends_on: Vec<TaskId>,
    pub status: LifecycleStatus,
    pub created_at: u64,
    pub updated_at: u64,
}

/// The only Task JSON that persistence may store.
///
/// `create_run`, idempotent creation, turn commit, task updates, and recovery
/// must all pass through this so a later Engine path cannot skip redaction.
pub fn safe_task_value(task: &Task) -> Result<Value> {
    Ok(crate::secrets::redact_persisted(&serde_json::to_value(
        task,
    )?))
}

/// Same key plus the same request digest returns the original turn.
/// Same key plus a different digest is a conflict and must not create another
/// turn, run, or task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdempotencyHit {
    Same,
    Conflict,
}

/// Explicit replay result.  An old run is not converted into a Turn because
/// the legacy table has no project, session, or agent identity.
#[derive(Clone, Debug)]
pub enum IdempotencyReplay {
    Turn(Turn),
    LegacyRun(Run),
}

/// Application command version included in the send-turn digest.
pub const SEND_CHAT_TURN_COMMAND: &str = "engine.send_chat_turn.v1";

/// One new user message on an existing, already completed, tool-free chat.
///
/// Identity fields are stable ids. Display names are not part of this command
/// and must not be used to decide session ownership.
#[derive(Clone, Debug)]
pub struct SendChatTurn {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    /// The caller-observed previous turn. The store rechecks that this is
    /// still the committed latest turn inside the append transaction.
    pub expected_last_turn_id: TurnId,
    pub message: String,
    pub idempotency_key: String,
}

/// The turn created or replayed by [`SendChatTurn`], not the whole legacy run.
#[derive(Clone, Debug)]
pub struct ChatTurnReceipt {
    pub turn: Turn,
    pub task: TurnTask,
    /// True when the key already owned this exact command and no new work ran.
    pub replayed: bool,
}

/// Stable, classifiable failures for appending a chat turn.
///
/// Callers must branch on this type. The display text is only for humans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatTurnError {
    /// The previous turn id is not the committed latest turn.
    StalePredecessor,
    /// The latest turn or its task is not completed, so a new turn cannot start.
    SessionBusy,
    /// The predecessor id is still latest, but its persisted content changed
    /// after the candidate was built. Replay of an already committed key is
    /// unaffected.
    PredecessorChanged,
    /// The session is not a single tool-free chat that this command can extend.
    UnsupportedSession,
    /// The message, key, or required identity failed validation.
    InvalidInput,
    /// A required session, agent, or turn row does not exist.
    NotFound,
    /// A required row exists but cannot be decoded, or ordering evidence is missing.
    CorruptState,
}

impl std::fmt::Display for ChatTurnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::StalePredecessor => "前序回合已不是最新提交回合",
            Self::SessionBusy => "当前回合尚未完成，不能追加新回合",
            Self::PredecessorChanged => "前序回合内容已变化，请按当前快照重试",
            Self::UnsupportedSession => "当前会话不支持无工具连续对话追加",
            Self::InvalidInput => "连续对话请求无效",
            Self::NotFound => "连续对话所需对象不存在",
            Self::CorruptState => "连续对话持久状态损坏，不能判断归属或顺序",
        })
    }
}

impl std::error::Error for ChatTurnError {}

/// Digest of the stable send-turn inputs. Generated ids, clocks, settings, and
/// runtime status are intentionally excluded.
pub fn send_chat_turn_hash(command: &SendChatTurn) -> Result<String> {
    let payload = serde_json::json!({
        "command": SEND_CHAT_TURN_COMMAND,
        "session_id": command.session_id.0,
        "agent_id": command.agent_id.0,
        "expected_last_turn_id": command.expected_last_turn_id.0,
        "message": command.message,
    });
    let digest = sha2::Sha256::digest(serde_json::to_vec(&payload)?);
    Ok(format!("{digest:x}"))
}

pub fn event_id(seq: i64) -> EventId {
    EventId(format!("evt-{seq}"))
}

pub fn stamped_event(
    seq: i64,
    session_id: &str,
    turn_id: Option<&str>,
    task_id: &str,
    kind: &str,
    data: Value,
    at: u64,
) -> Event {
    Event {
        schema_version: EVENT_SCHEMA_VERSION,
        seq,
        cursor: seq.to_string(),
        session_id: Some(session_id.to_owned()),
        turn_id: turn_id.map(str::to_owned),
        task_id: task_id.to_owned(),
        kind: kind.to_owned(),
        data,
        at,
    }
}

pub fn validate_event_kind(kind: &str) -> Result<()> {
    ensure!(!kind.is_empty() && kind.len() <= 96, "事件 kind 无效");
    ensure!(
        !kind.chars().any(|ch| ch.is_control()),
        "事件 kind 包含控制字符"
    );
    const LEGACY: &[&str] = &[
        "status",
        "delta",
        "error",
        "persistence_error",
        "tool_start",
        "tool_result",
        "file_backup",
        "file_restore",
        "resume",
    ];
    const CORE: &[&str] = &[
        "turn.created",
        "turn.status",
        "task.created",
        "task.status",
        "approval.requested",
        "approval.resolved",
        "tool.started",
        "tool.finished",
        "file.backup",
        "file.restored",
    ];
    let safe_unknown = kind.strip_prefix("unknown:").is_some_and(|name| {
        !name.is_empty()
            && name.as_bytes()[0].is_ascii_lowercase()
            && name
                .bytes()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'_')
    });
    ensure!(
        LEGACY.contains(&kind) || CORE.contains(&kind) || safe_unknown,
        "事件 kind 不在已声明契约中"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths_and_names() {
        assert!(overlaps("src", "src/main.rs"));
        assert!(!overlaps("src/a", "src/abc"));
        assert!(scope_path("../secret").is_err());
        assert!(scope_path("C:\\secret").is_err());
        assert!(scope_path("src/../secret").is_err());
        assert!(!valid_name("Lead Name"));
        assert!(valid_name("api-worker"));
    }

    #[test]
    fn session_kind_is_explicit_and_name_independent() {
        let chat: RunRequest = serde_json::from_value(serde_json::json!({
            "kind": "chat",
            "title": "debug",
            "tasks": [{
                "name": "任意显示名",
                "role": "对话助手",
                "route_id": "route",
                "prompt": "检查项目"
            }]
        }))
        .unwrap();
        assert_eq!(chat.kind, SessionKind::Chat);
        assert_eq!(chat.tasks[0].name, "任意显示名");

        let team: RunRequest = serde_json::from_value(serde_json::json!({
            "title": "team",
            "tasks": [{
                "name": "chat-session",
                "role": "普通成员",
                "route_id": "route",
                "prompt": "执行团队任务"
            }]
        }))
        .unwrap();
        assert_eq!(team.kind, SessionKind::Team);
    }

    #[test]
    fn project_session_turn_ids_do_not_follow_display_names() {
        let project = Project {
            id: ProjectId::from("prj-1"),
            name: "任意项目名".into(),
            root_path: "D:/work".into(),
            created_at: 1,
            updated_at: 1,
        };
        let session = Session {
            id: SessionId::from("ses-1"),
            project_id: project.id.clone(),
            kind: SessionKind::Chat,
            title: "chat-session".into(),
            legacy_run_id: "ses-1".into(),
            created_at: 1,
            updated_at: 1,
        };
        let turn = Turn {
            id: TurnId::from("trn-1"),
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            status: LifecycleStatus::Queued,
            request_hash: "digest".into(),
            idempotency_key: Some("idem-1".into()),
            created_at: 1,
            updated_at: 1,
        };
        assert_eq!(session.kind, SessionKind::Chat);
        assert_ne!(session.id.0, session.title);
        assert_ne!(turn.id.0, "chat-session");
        assert_eq!(serde_json::to_value(&session).unwrap()["kind"], "chat");
        let event = stamped_event(
            4,
            &session.id.0,
            Some(&turn.id.0),
            "task-1",
            "status",
            json_status(),
            9,
        );
        assert_eq!(event.cursor, "4");
        assert_eq!(event.schema_version, EVENT_SCHEMA_VERSION);
        assert_eq!(event.envelope().timestamp, 9);
        assert_eq!(event_id(4).0, "evt-4");
    }

    fn json_status() -> Value {
        serde_json::json!({"status": "queued"})
    }
}
