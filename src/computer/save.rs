use super::*;

#[derive(Debug)]
pub(crate) struct SaveNotDispatched;

impl std::fmt::Display for SaveNotDispatched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Save As was not dispatched; re-observe the foreground dialog and review the destination before trying again")
    }
}

impl ComputerService {
    /// Called only after approval. The observation grants one attempt, even if a
    /// provider reports failure after having partially handled the UI action.
    pub(crate) async fn save_as(
        &self,
        session: &str,
        workspace: &Path,
        observation_ref: &str,
        target: &Path,
    ) -> Result<()> {
        let reg = self
            .for_workspace(workspace)
            .context("no computer target registered for this workspace")?
            .clone();
        let reference = observation_ref.to_owned();
        let session = session.to_owned();
        let target = target.to_owned();
        tokio::task::spawn_blocking(move || reg.dispatch_save(&session, &reference, &target))
            .await?
    }
}

impl Registration {
    fn dispatch_save(&self, session: &str, reference: &str, target: &Path) -> Result<()> {
        let mut history = self
            .observations
            .lock()
            .expect("computer observations poisoned");
        let (hwnd, controls, path) = self
            .prepare_save(&mut history, session, reference, target)
            .context(SaveNotDispatched)?;
        // Only preflight errors above guarantee that neither control was touched.
        self.target.save_as(hwnd, &controls, &path)
    }

    fn prepare_save(
        &self,
        history: &mut VecDeque<Observation>,
        session: &str,
        reference: &str,
        target: &Path,
    ) -> Result<(usize, SaveControls, PathBuf)> {
        ensure!(
            self.allow_save_as,
            "target registration is observation-only"
        );
        let observed = history
            .iter_mut()
            .find(|o| o.session == session && o.reference == reference)
            .context("stale or unknown observation_ref for this session")?;
        let (generation, controls) = observed.save.take().context("no unused save capability")?;
        let WindowState::Target {
            hwnd, dialog: true, ..
        } = observed.state
        else {
            bail!("not an observed save dialog");
        };
        ensure!(
            self.target.focus_epoch() == generation,
            "foreground changed since observation"
        );
        ensure!(
            matches!(self.target.snapshot()?, WindowState::Target { hwnd: current, dialog: true, .. } if current == hwnd),
            "save dialog is no longer foreground"
        );
        ensure!(
            self.target.save_controls(hwnd)?.as_ref() == Some(&controls),
            "save controls changed"
        );
        let path = new_text_path(&self.workspace, target)?;
        ensure!(
            self.target.focus_epoch() == generation,
            "foreground changed during validation"
        );
        Ok((hwnd, controls, path))
    }
}

fn new_text_path(root: &Path, target: &Path) -> Result<PathBuf> {
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .context("expected a text filename")?;
    let parent = target.parent().context("expected a workspace filename")?;
    if target.is_absolute() {
        ensure!(
            parent.canonicalize()? == root,
            "expected a file directly in the registered workspace"
        );
    } else {
        ensure!(target.as_os_str() == name, "expected a single filename");
    }
    let stem = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.eq_ignore_ascii_case("txt"))
        .map(|(stem, _)| stem)
        .context("expected .txt filename")?;
    ensure!(
        !stem.trim_matches([' ', '.']).is_empty()
            && !name
                .chars()
                .any(|c| c.is_control() || r#"<>:"/\|?*"#.contains(c)),
        "invalid text filename"
    );
    let device = stem
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    ensure!(
        !matches!(
            device.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) && !["COM", "LPT"]
            .iter()
            .any(|prefix| device
                .strip_prefix(prefix)
                .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit()
                    || matches!(n, "¹" | "²" | "³"))),
        "reserved device filename"
    );
    let path = root.join(name);
    match path.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(e) => Err(e.into()),
        Ok(_) => bail!("save target already exists"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

    struct Dialog {
        epoch: AtomicU64,
        control: AtomicU64,
        calls: AtomicU64,
        state: Mutex<WindowState>,
    }
    impl Target for Arc<Dialog> {
        fn focus_epoch(&self) -> u64 {
            self.epoch.load(SeqCst)
        }
        fn snapshot(&self) -> Result<WindowState> {
            Ok(self.state.lock().unwrap().clone())
        }
        fn subscribe(&self) -> watch::Receiver<u64> {
            watch::channel(0).1
        }
        fn save_controls(&self, _: usize) -> Result<Option<SaveControls>> {
            Ok(Some(SaveControls {
                filename_id: vec![self.control.load(SeqCst) as i32],
                save_id: vec![7],
            }))
        }
        fn save_as(&self, _: usize, _: &SaveControls, _: &Path) -> Result<()> {
            self.calls.fetch_add(1, SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn save_capability_is_scoped_fresh_and_single_use() -> Result<()> {
        let root = tempfile::tempdir()?;
        let other = tempfile::tempdir()?;
        let path = root.path();
        let out = path.join("test.txt");
        let initial = WindowState::Target {
            hwnd: 42,
            class: "#32770".into(),
            title: "Save As".into(),
            dialog: true,
        };
        let fake = Arc::new(Dialog {
            epoch: 0.into(),
            control: 1.into(),
            calls: 0.into(),
            state: Mutex::new(initial.clone()),
        });
        let mut svc = ComputerService {
            registration: Some(Arc::new(Registration {
                workspace: path.canonicalize()?,
                allow_save_as: false,
                target: Box::new(fake.clone()),
                observations: Mutex::default(),
            })),
        };
        let cancel = CancellationToken::new();
        let readonly = svc
            .execute("a", path, "computer_observe", &json!({}), &cancel)
            .await?;
        assert!(readonly.get("save_as_available").is_none());
        assert!(!svc.save_enabled_for(path));
        assert!(
            svc.save_as(
                "a",
                path,
                readonly["observation_ref"].as_str().unwrap(),
                &out
            )
            .await
            .is_err()
        );
        Arc::get_mut(svc.registration.as_mut().unwrap())
            .unwrap()
            .allow_save_as = true;
        let observe = || async {
            let obs = svc
                .execute("a", path, "computer_observe", &json!({}), &cancel)
                .await
                .unwrap();
            assert_eq!(obs["save_as_available"], true);
            obs["observation_ref"].as_str().unwrap().to_owned()
        };
        let old = observe().await;
        let r = observe().await;
        assert!(
            svc.execute(
                "a",
                path,
                "computer_save_as",
                &json!({"observation_ref":r,"path":"test.txt","expected_text":"ok"}),
                &cancel
            )
            .await
            .is_err()
        );
        assert!(svc.save_as("a", path, &old, &out).await.is_err());
        assert!(svc.save_as("a", path, "forged", &out).await.is_err());
        assert!(svc.save_as("b", path, &r, &out).await.is_err());
        assert!(svc.save_as("a", other.path(), &r, &out).await.is_err());
        // Two focus transitions return to the same HWND and controls.
        fake.epoch.fetch_add(2, SeqCst);
        assert!(svc.save_as("a", path, &r, &out).await.is_err());
        let r = observe().await;
        fake.control.fetch_add(1, SeqCst);
        assert!(svc.save_as("a", path, &r, &out).await.is_err());
        for state in [WindowState::Outside, WindowState::Exited] {
            let r = observe().await;
            *fake.state.lock().unwrap() = state;
            assert!(svc.save_as("a", path, &r, &out).await.is_err());
            *fake.state.lock().unwrap() = initial.clone();
        }
        let r = observe().await;
        assert_eq!(fake.calls.load(SeqCst), 0);
        if let WindowState::Target { title, .. } = &mut *fake.state.lock().unwrap() {
            title.push('*'); // Text changes do not imply a focus transition.
        }
        svc.save_as("a", path, &r, &out).await?;
        assert!(svc.save_as("a", path, &r, &out).await.is_err());
        assert_eq!(fake.calls.load(SeqCst), 1);
        Ok(())
    }

    #[test]
    fn only_new_workspace_text_files_are_accepted() -> Result<()> {
        let root = tempfile::tempdir()?;
        let path = root.path().canonicalize()?;
        std::fs::write(path.join("existing.txt"), "original")?;
        for name in [
            "existing.txt",
            "../other.txt",
            "sub/file.txt",
            ".txt",
            "CON.hello.txt",
            "lpt².txt",
            "file:stream.txt",
            "a.exe",
            "*.txt",
        ] {
            assert!(new_text_path(&path, Path::new(name)).is_err(), "{name}");
        }
        assert_eq!(
            new_text_path(&path, Path::new("笔记.txt"))?,
            path.join("笔记.txt")
        );
        Ok(())
    }
}
