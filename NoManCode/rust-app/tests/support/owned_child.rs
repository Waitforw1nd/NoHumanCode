//! Original-child ownership for isolated integration fixtures.
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

pub struct OwnedChild {
    child: Child,
    evidence: PathBuf,
}
impl OwnedChild {
    pub fn spawn(command: &mut Command, evidence: &Path) -> Self {
        fs::create_dir_all(evidence).unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        command
            .stdin(Stdio::null())
            .stdout(fs::File::create(evidence.join("stdout.log")).unwrap())
            .stderr(fs::File::create(evidence.join("stderr.log")).unwrap());
        Self {
            child: command.spawn().expect("spawn original fixture child"),
            evidence: evidence.to_owned(),
        }
    }
    #[allow(dead_code)]
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }
    #[allow(dead_code)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
    fn retain(&self, reason: &str) -> ! {
        let _ = fs::write(self.evidence.join("unknown.txt"), reason);
        eprintln!(
            "NHC_TERMINAL_UNKNOWN: {reason}; original Child retained at {}",
            self.evidence.display()
        );
        loop {
            std::thread::park_timeout(Duration::from_secs(60));
        }
    }
    pub fn output(mut self, timeout: Duration) -> Output {
        let deadline = Instant::now() + timeout;
        let status = loop {
            if Instant::now() >= deadline {
                self.retain("child exit not observed within deadline; retain original child");
            }
            match self.child.try_wait() {
                Ok(Some(status)) if Instant::now() < deadline => break status,
                Ok(Some(_)) => self.retain("child exit was observed after deadline"),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                _ => self.retain("child exit unconfirmed; do not kill nested unknown evidence"),
            }
        };
        Output {
            status,
            stdout: fs::read(self.evidence.join("stdout.log")).unwrap(),
            stderr: fs::read(self.evidence.join("stderr.log")).unwrap(),
        }
    }
    pub fn kill_wait(&mut self) -> ExitStatus {
        match self.child.try_wait() {
            Ok(Some(status)) => return status,
            Ok(None) => {}
            Err(_) => self.retain("cannot observe original child before cleanup"),
        }
        if self.child.kill().is_err() {
            // An exit racing the exact-handle kill is safe only if observed.
            return match self.child.try_wait() {
                Ok(Some(status)) => status,
                _ => self.retain("original child termination unconfirmed"),
            };
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if Instant::now() >= deadline {
                self.retain("original child signal not observed within termination deadline");
            }
            match self.child.try_wait() {
                Ok(Some(status)) if Instant::now() < deadline => return status,
                Ok(Some(_)) => {
                    self.retain("original child signal observed after termination deadline")
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                _ => self.retain("original child signal after termination unconfirmed"),
            }
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.kill_wait();
    }
}
