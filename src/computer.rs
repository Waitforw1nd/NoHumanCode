use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[cfg(windows)]
mod windows;

#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(super) enum WindowState {
    Outside,
    Exited,
    Target {
        hwnd: usize,
        class: String,
        title: String,
        dialog: bool,
    },
}

trait Target: Send + Sync {
    fn snapshot(&self) -> Result<WindowState>;
    fn subscribe(&self) -> watch::Receiver<u64>;
}

#[cfg(windows)]
impl Target for windows::NativeTarget {
    fn snapshot(&self) -> Result<WindowState> {
        self.snapshot()
    }
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.subscribe()
    }
}

struct Observation {
    session: String,
    reference: String,
    state: WindowState,
}

struct Registration {
    workspace: PathBuf,
    target: Box<dyn Target>,
    observations: Mutex<VecDeque<Observation>>,
}

#[derive(Default)]
pub struct ComputerService {
    registration: Option<Registration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Condition {
    TargetForeground,
    DialogForeground,
    Changed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wait {
    condition: Condition,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    observation_ref: Option<String>,
}

fn default_timeout() -> u64 {
    10_000
}

impl ComputerService {
    pub fn from_env() -> Result<Self> {
        let Some(raw) = std::env::var_os("CODA_COMPUTER_TARGET") else {
            return Ok(Self::default());
        };
        #[cfg(not(windows))]
        {
            let _ = raw;
            bail!("CODA_COMPUTER_TARGET requires Windows");
        }
        #[cfg(windows)]
        {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Config {
                workspace: PathBuf,
                pid: u32,
            }
            let config: Config = serde_json::from_str(
                raw.to_str()
                    .context("invalid CODA_COMPUTER_TARGET encoding")?,
            )
            .context("invalid CODA_COMPUTER_TARGET registration")?;
            ensure!(config.pid != 0, "target PID must be positive");
            ensure!(
                config.workspace.is_absolute(),
                "registered workspace must be absolute"
            );
            let workspace = config
                .workspace
                .canonicalize()
                .context("cannot resolve registered workspace")?;
            ensure!(
                workspace.is_dir(),
                "registered workspace must be a directory"
            );
            Ok(Self {
                registration: Some(Registration {
                    workspace,
                    target: Box::new(windows::NativeTarget::open(config.pid)?),
                    observations: Mutex::default(),
                }),
            })
        }
    }

    fn for_workspace(&self, workspace: &Path) -> Option<&Registration> {
        let registration = self.registration.as_ref()?;
        if workspace.is_absolute() && workspace.canonicalize().ok()? == registration.workspace {
            Some(registration)
        } else {
            None
        }
    }

    pub(crate) fn enabled_for(&self, workspace: &Path) -> bool {
        self.for_workspace(workspace).is_some()
    }

    pub async fn execute(
        &self,
        session: &str,
        workspace: &Path,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        let reg = self
            .for_workspace(workspace)
            .context("no computer target registered for this workspace")?;
        ensure!(!cancel.is_cancelled(), "computer observation cancelled");
        let args = args
            .as_object()
            .context("expected a tool argument object")?;
        let wait = match name {
            "computer_observe" => {
                ensure!(args.is_empty(), "computer_observe takes no arguments");
                None
            }
            "computer_wait" => {
                ensure!(
                    args.values().all(|v| !v.is_null()),
                    "unexpected null argument"
                );
                let wait: Wait = serde_json::from_value(Value::Object(args.clone()))?;
                ensure!(
                    (1..=60_000).contains(&wait.timeout_ms),
                    "timeout_ms must be between 1 and 60000"
                );
                ensure!(
                    matches!(wait.condition, Condition::Changed) == wait.observation_ref.is_some(),
                    "observation_ref is required exclusively for changed"
                );
                Some(wait)
            }
            _ => bail!("unknown computer tool"),
        };
        let deadline =
            Instant::now() + Duration::from_millis(wait.as_ref().map_or(0, |w| w.timeout_ms));
        let mut events = reg.target.subscribe();
        loop {
            // Mark events seen before sampling. An event during snapshot stays pending.
            events.borrow_and_update();
            {
                let mut history = reg
                    .observations
                    .lock()
                    .expect("computer observations poisoned");
                let state = reg.target.snapshot()?;
                ensure!(!cancel.is_cancelled(), "computer observation cancelled");
                let outcome = match &wait {
                    Some(wait) => wait.outcome(session, &history, &state, deadline)?,
                    None => None,
                };
                if wait.is_none() || outcome.is_some() {
                    return Ok(publish(session, state, outcome, &mut history));
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => bail!("computer observation cancelled"),
                _ = tokio::time::sleep_until(deadline) => {},
                status = events.changed() => status.context("computer notification service stopped")?,
            }
        }
    }
}

impl Wait {
    fn outcome(
        &self,
        session: &str,
        history: &VecDeque<Observation>,
        state: &WindowState,
        deadline: Instant,
    ) -> Result<Option<&'static str>> {
        let matched = match self.condition {
            Condition::Changed => {
                let latest = history
                    .iter()
                    .find(|o| o.session == session)
                    .filter(|o| Some(&o.reference) == self.observation_ref.as_ref())
                    .context("stale or unknown observation_ref for this session")?;
                latest.state != *state
            }
            Condition::TargetForeground => matches!(state, WindowState::Target { .. }),
            Condition::DialogForeground => {
                matches!(state, WindowState::Target { dialog: true, .. })
            }
        };
        Ok(if *state == WindowState::Exited {
            Some("target_exited")
        } else if matched {
            Some("matched")
        } else if Instant::now() >= deadline {
            Some("timeout")
        } else {
            None
        })
    }
}

fn publish(
    session: &str,
    state: WindowState,
    outcome: Option<&str>,
    history: &mut VecDeque<Observation>,
) -> Value {
    const MAX_SESSIONS: usize = 256;
    let reference = Uuid::new_v4().to_string();
    let mut result = match &state {
        WindowState::Outside => json!({"state":"outside_target"}),
        WindowState::Exited => json!({"state":"target_exited"}),
        WindowState::Target {
            class,
            title,
            dialog,
            ..
        } => json!({
            "state": if *dialog {"dialog_foreground"} else {"target_foreground"},
            "class": class, "title": title,
        }),
    };
    result["observation_ref"] = json!(reference);
    if let Some(outcome) = outcome {
        result["outcome"] = json!(outcome);
    }
    history.retain(|o| o.session != session);
    if history.len() >= MAX_SESSIONS {
        history.pop_front();
    }
    history.push_back(Observation {
        session: session.to_owned(),
        reference,
        state,
    });
    result
}

pub(crate) fn definitions() -> Vec<Value> {
    let tool = |name, description, properties, required| {
        json!({
            "type": "function", "function": {"name": name, "description": description,
            "parameters": {"type":"object","properties":properties,"required":required,"additionalProperties":false}}
        })
    };
    vec![
        tool(
            "computer_observe",
            "Read the registered application's current foreground state. Returns its title and class only while it is foreground, plus an observation_ref for the next wait.",
            json!({}),
            json!([]),
        ),
        tool(
            "computer_wait",
            "Wait locally until the registered application is foreground (including its dialogs), a standard dialog is foreground, or the observed state changes. For changed, supply this session's latest observation_ref. Returns on match, timeout or target exit.",
            json!({
                "condition":{"type":"string","enum":["target_foreground","dialog_foreground","changed"]},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":60000,"default":default_timeout()},
                "observation_ref":{"type":"string","description":"Required only for changed."}
            }),
            json!(["condition"]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    struct Fake {
        state: Mutex<WindowState>,
        during_snapshot: Mutex<Option<WindowState>>,
        events: watch::Sender<u64>,
    }

    impl Fake {
        fn set(&self, state: WindowState) {
            *self.state.lock().unwrap() = state;
            self.events.send_modify(|epoch| *epoch += 1);
        }
    }

    impl Target for Arc<Fake> {
        fn snapshot(&self) -> Result<WindowState> {
            let state = self.state.lock().unwrap().clone();
            if let Some(next) = self.during_snapshot.lock().unwrap().take() {
                self.set(next);
            }
            Ok(state)
        }
        fn subscribe(&self) -> watch::Receiver<u64> {
            self.events.subscribe()
        }
    }

    fn foreground(dialog: bool) -> WindowState {
        WindowState::Target {
            hwnd: 42,
            class: "FixtureWindow".into(),
            title: "Fixture".into(),
            dialog,
        }
    }

    impl ComputerService {
        pub(crate) fn fixture(workspace: &Path) -> Self {
            fake_service(workspace).0
        }
    }

    fn fake_service(workspace: &Path) -> (ComputerService, Arc<Fake>) {
        let fake = Arc::new(Fake {
            state: Mutex::new(foreground(false)),
            during_snapshot: Mutex::new(None),
            events: watch::channel(0).0,
        });
        let svc = ComputerService {
            registration: Some(Registration {
                workspace: workspace.canonicalize().unwrap(),
                target: Box::new(fake.clone()),
                observations: Mutex::default(),
            }),
        };
        (svc, fake)
    }

    #[tokio::test]
    async fn sees_event_during_initial_snapshot_and_already_open_dialog() {
        let root = tempfile::tempdir().unwrap();
        let (svc, fake) = fake_service(root.path());
        *fake.during_snapshot.lock().unwrap() = Some(foreground(true));
        let cancel = CancellationToken::new();
        let args = json!({"condition":"dialog_foreground","timeout_ms":5000});
        for _ in 0..2 {
            let mut call = Box::pin(svc.execute("a", root.path(), "computer_wait", &args, &cancel));
            // Both the racing event and the subsequent level check complete without yielding.
            let std::task::Poll::Ready(result) = futures_util::poll!(&mut call) else {
                panic!("foreground event was missed");
            };
            let result = result.unwrap();
            assert_eq!(result["outcome"], "matched");
            assert_eq!(result["state"], "dialog_foreground");
            assert!(result.get("hwnd").is_none());
        }
    }

    #[tokio::test]
    async fn changes_are_scoped_to_workspace_host_and_latest_session_reference() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = root.path();
        let (svc, fake) = fake_service(path);
        let cancel = CancellationToken::new();
        let observe = json!({});
        let initial = svc
            .execute("a", path, "computer_observe", &observe, &cancel)
            .await
            .unwrap();
        let changed = json!({"condition":"changed","observation_ref":initial["observation_ref"]});
        let restarted = ComputerService::fixture(path);
        assert!(
            restarted
                .execute("a", path, "computer_wait", &changed, &cancel)
                .await
                .is_err()
        );
        assert!(
            svc.execute("b", path, "computer_wait", &changed, &cancel)
                .await
                .is_err()
        );
        assert!(
            svc.execute("a", other.path(), "computer_wait", &changed, &cancel)
                .await
                .is_err()
        );
        let mut waiting = Box::pin(svc.execute("a", path, "computer_wait", &changed, &cancel));
        assert!(futures_util::poll!(&mut waiting).is_pending());
        svc.execute("b", path, "computer_observe", &observe, &cancel)
            .await
            .unwrap();
        fake.set(WindowState::Outside);
        let next = waiting.await.unwrap();
        assert_eq!(next["state"], "outside_target");
        assert!(next.get("title").is_none() && next.get("class").is_none());
        assert!(
            svc.execute("a", path, "computer_wait", &changed, &cancel)
                .await
                .is_err()
        );
        let current = json!({"condition":"changed","observation_ref":next["observation_ref"]});
        let mut pending = Box::pin(svc.execute("a", path, "computer_wait", &current, &cancel));
        assert!(futures_util::poll!(&mut pending).is_pending());
        let latest = svc
            .execute("a", path, "computer_observe", &observe, &cancel)
            .await
            .unwrap();
        fake.set(foreground(false));
        assert!(pending.await.is_err());
        let current = json!({"condition":"changed","observation_ref":latest["observation_ref"]});
        assert_eq!(
            svc.execute("a", path, "computer_wait", &current, &cancel)
                .await
                .unwrap()["outcome"],
            "matched"
        );
    }

    #[tokio::test]
    async fn rejects_invalid_input_and_closed_notification_stream() {
        struct Disconnected;
        impl Target for Disconnected {
            fn snapshot(&self) -> Result<WindowState> {
                Ok(WindowState::Outside)
            }
            fn subscribe(&self) -> watch::Receiver<u64> {
                watch::channel(0).1
            }
        }
        let root = tempfile::tempdir().unwrap();
        let mut service = ComputerService::fixture(root.path());
        service.registration.as_mut().unwrap().target = Box::new(Disconnected);
        let cancel = CancellationToken::new();
        for args in [
            json!({"condition":"changed"}),
            json!({"condition":"target_foreground","observation_ref":"unrelated"}),
            json!({"condition":"dialog_foreground","timeout_ms":60001}),
            json!({"condition":"dialog_foreground","timeout_ms":0}),
            json!({"condition":"dialog_foreground","pid":42}),
            json!({"condition":"click"}),
        ] {
            assert!(
                service
                    .execute("a", root.path(), "computer_wait", &args, &cancel)
                    .await
                    .is_err()
            );
        }
        let args = json!({"condition":"dialog_foreground"});
        let error = service
            .execute("a", root.path(), "computer_wait", &args, &cancel)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("notification service stopped"));
    }

    #[tokio::test]
    async fn timeout_cancellation_and_exit_finish_wait() {
        let root = tempfile::tempdir().unwrap();
        let (svc, fake) = fake_service(root.path());
        let wait = json!({"condition":"dialog_foreground","timeout_ms":1});
        let cancel = CancellationToken::new();
        let result = svc
            .execute("a", root.path(), "computer_wait", &wait, &cancel)
            .await
            .unwrap();
        assert_eq!(result["outcome"], "timeout");
        let args = json!({"condition":"dialog_foreground"});
        let mut pending = Box::pin(svc.execute("a", root.path(), "computer_wait", &args, &cancel));
        assert!(futures_util::poll!(&mut pending).is_pending());
        cancel.cancel();
        assert!(pending.await.is_err());
        let cancel = CancellationToken::new();
        let mut pending = Box::pin(svc.execute("a", root.path(), "computer_wait", &args, &cancel));
        assert!(futures_util::poll!(&mut pending).is_pending());
        fake.set(WindowState::Exited);
        let result = pending.await.unwrap();
        assert_eq!(result["outcome"], "target_exited");
        assert_eq!(result["state"], "target_exited");
    }
}
