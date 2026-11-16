use super::fixture::{Case, until};
use anyhow::{Context, Result, ensure};
use axum::{Json, Router, extract::State, routing::post};
use peachsh::{
    approval::{ApprovalRecord, ExecutionState},
    domain::{Route, RunRequest, SessionKind, Settings, Task, TaskSpec},
    engine::Engine,
    store::Store,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[path = "../network_fixture.rs"]
mod network_fixture;

type Script = Arc<Mutex<VecDeque<Value>>>;
async fn completion(
    State(script): State<Script>,
    Json(_): Json<Value>,
) -> ([(&'static str, &'static str); 1], String) {
    let delta = match script.lock().unwrap().pop_front() {
        Some(call) => json!({"tool_calls":[call]}),
        None => json!({"content":"done"}),
    };
    let chunk = json!({"choices":[{"delta":delta,"finish_reason":"stop"}]});
    (
        [("content-type", "text/event-stream")],
        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
    )
}

pub struct Host {
    pub engine: Arc<Engine>,
    pub database: PathBuf,
    script: Script,
    server: tokio::task::JoinHandle<()>,
}
impl Host {
    pub async fn new(case: &Case) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}/v1", listener.local_addr()?);
        let script = Script::default();
        let app = Router::new()
            .route("/v1/chat/completions", post(completion))
            .with_state(script.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let database = case.root.join("lifecycle.sqlite3");
        let store = Arc::new(Store::open(&database)?);
        store.save_settings(&Settings {
            workspace: case.root.to_string_lossy().into(),
            max_concurrency: 2,
            routes: vec![Route {
                id: "route".into(),
                name: "lifecycle".into(),
                base_url: base,
                model: "fixture".into(),
                max_tokens: 128,
                parallel_limit: 2,
                key_env: None,
            }],
            newapi: None,
        })?;
        store.put_secret("route::route", "lifecycle-local-fixture")?;
        network_fixture::authorize_routes(&store);
        let engine = Engine::new(store, 2)?;
        Ok(Self {
            engine,
            database,
            script,
            server,
        })
    }
    pub fn request() -> RunRequest {
        RunRequest {
            title: "lifecycle".into(),
            kind: SessionKind::Team,
            tasks: vec![TaskSpec {
                name: "worker".into(),
                role: "worker".into(),
                route_id: "route".into(),
                prompt: "exercise the authorized command".into(),
                depends_on: vec![],
                write_scopes: vec!["*".into()],
                tools: true,
                allow_commands: true,
                max_rounds: 4,
            }],
        }
    }
    pub async fn shell(
        &self,
        command: &str,
        timeout_seconds: u64,
    ) -> Result<(Task, ApprovalRecord)> {
        let id = uuid::Uuid::new_v4().to_string();
        self.script.lock().unwrap().push_back(json!({"index":0,"id":id,"type":"function",
            "function":{"name":"run_command","arguments":json!({"command":command,"timeout_seconds":timeout_seconds}).to_string()}}));
        let task = self.engine.start(Self::request()).await?.tasks.remove(0);
        let mut approval = None;
        until(|| {
            approval = self
                .engine
                .store
                .pending_approvals_for_task(&task.id)?
                .into_iter()
                .next();
            Ok(approval.is_some())
        })
        .await?;
        let approval = approval.context("approval not observed")?;
        ensure!(approval.tool_name == "run_command", "wrong approval tool");
        self.engine.decide_approval_with_network_ack(
            &approval.id,
            true,
            Some("lifecycle-test"),
            true,
        )?;
        Ok((task, approval))
    }
    pub async fn finished(&self, task: &Task) -> Result<Task> {
        until(|| {
            let current = self.engine.store.task(&task.id)?;
            Ok(!self.engine.is_busy() && !matches!(current.status.as_str(), "queued" | "running"))
        })
        .await?;
        self.engine.store.task(&task.id)
    }
    pub async fn success(&self) -> Result<()> {
        let (task, approval) = self
            .shell("[Console]::Out.Write('lifecycle-success'); exit 0", 10)
            .await?;
        let task = self.finished(&task).await?;
        ensure!(
            self.engine.store.approval(&approval.id)?.execution_state == ExecutionState::Finished,
            "success was not durably finished"
        );
        ensure!(
            task.messages.iter().any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("lifecycle-success"))),
            "real pwsh output missing"
        );
        Ok(())
    }
    pub async fn close(&self) -> Result<()> {
        let report = self.engine.shutdown(Duration::from_secs(10)).await;
        ensure!(report.settled(), "Host shutdown not settled: {report:?}");
        Ok(())
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.engine.request_shutdown();
        self.server.abort();
    }
}
