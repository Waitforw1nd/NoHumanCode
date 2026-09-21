use anyhow::{Result, bail, ensure};
pub use peachsh_protocol::SessionKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

#[derive(Clone, Serialize, Deserialize, Debug)]
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

#[derive(Clone, Serialize, Deserialize)]
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
#[derive(Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub kind: SessionKind,
    pub created_at: u64,
    pub tasks: Vec<Task>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: i64,
    pub task_id: String,
    pub kind: String,
    pub data: Value,
    pub at: u64,
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
}
