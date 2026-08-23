//! The host itself: `tokio::process` plus the host filesystem.
//!
//! This is the floor and the default. Its behaviour is deliberately bit-for-bit
//! what the supervisor did inline before the seam existed — process groups,
//! `killpg`, the same environment variables — so the existing integration suite
//! is the regression test for the refactor rather than something that had to be
//! rewritten alongside it.

use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use nix::sys::signal::{killpg, Signal as NixSignal};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tracing::debug;

use super::{
    Environment, EnvironmentDescriptor, Health, ProcessHandle, ProcessSignal, ProcessSpec,
    Signal,
};

pub struct LocalEnvironment;

impl LocalEnvironment {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalEnvironment {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Environment for LocalEnvironment {
    fn descriptor(&self) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            kind: "local".to_string(),
            id: "local".to_string(),
            label: "This machine".to_string(),
        }
    }

    async fn spawn(&self, spec: ProcessSpec) -> Result<Box<dyn ProcessHandle>> {
        let mut cmd = Command::new(&spec.program);
        // Piped either way. `duplex_stdin` is not a spawn-time difference: it
        // tells the *caller* whether to keep the handle or drop it after one
        // write, which is the v2/v1 distinction. Branching here would suggest
        // the environment enforces it, and it does not.
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // A new process group is what lets a stop signal reach the CLI the
            // adapter exec'd rather than only the wrapper script.
            .process_group(0);

        for (key, value) in &spec.env {
            cmd.env(key, value);
        }

        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn {:?}", spec.program))?;

        // Captured at spawn, before anything can reap the child: `Child::id()`
        // returns `None` once it has been waited on, and the signaller has to
        // stay addressable for its whole life — including the window where the
        // reader task is awaiting `wait()`.
        let pid = child
            .id()
            .context("Process exited before its pid was available")?;

        Ok(Box::new(LocalProcess {
            child,
            signaler: Arc::new(LocalSignal { pgid: pid as i32 }),
        }))
    }

    async fn is_dir(&self, path: &str) -> Result<bool> {
        Ok(tokio::fs::metadata(path)
            .await
            .map(|meta| meta.is_dir())
            .unwrap_or(false))
    }

    async fn probe(&self) -> Health {
        // The host is reachable by definition: this code is running on it.
        Health::up()
    }
}

struct LocalProcess {
    child: Child,
    signaler: Arc<dyn ProcessSignal>,
}

/// Signals the process group by id.
///
/// Holding the pgid rather than the `Child` is what lets a stop request signal a
/// process whose output another task is currently draining.
struct LocalSignal {
    pgid: i32,
}

#[async_trait]
impl ProcessSignal for LocalSignal {
    async fn signal(&self, signal: Signal) -> Result<()> {
        let nix_signal = match signal {
            Signal::Term => NixSignal::SIGTERM,
            Signal::Kill => NixSignal::SIGKILL,
            Signal::Int => NixSignal::SIGINT,
        };

        // ESRCH means the group exited between the decision to signal and the
        // signal itself, which is a benign race — whoever is awaiting the exit
        // records the outcome. Reporting it as an error would make a stop that
        // arrived slightly late look like a failure to stop.
        if let Err(error) = killpg(Pid::from_raw(self.pgid), nix_signal) {
            debug!("{:?} to pgid {} failed: {}", signal, self.pgid, error);
        }
        Ok(())
    }
}

#[async_trait]
impl ProcessHandle for LocalProcess {
    fn take_stdin(&mut self) -> Option<Box<dyn AsyncWrite + Send + Unpin>> {
        self.child
            .stdin
            .take()
            .map(|stdin| Box::new(stdin) as Box<dyn AsyncWrite + Send + Unpin>)
    }

    fn take_stdout(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.child
            .stdout
            .take()
            .map(|stdout| Box::new(stdout) as Box<dyn AsyncRead + Send + Unpin>)
    }

    fn take_stderr(&mut self) -> Option<Box<dyn AsyncRead + Send + Unpin>> {
        self.child
            .stderr
            .take()
            .map(|stderr| Box::new(stderr) as Box<dyn AsyncRead + Send + Unpin>)
    }

    fn signaler(&self) -> Arc<dyn ProcessSignal> {
        self.signaler.clone()
    }

    async fn wait(&mut self) -> Result<Option<i32>> {
        let status = self.child.wait().await.context("Failed to await exit")?;
        Ok(status.code())
    }

    fn id(&self) -> Option<u32> {
        self.child.id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn spec(program: &str, args: &[&str], cwd: &str) -> ProcessSpec {
        ProcessSpec {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: cwd.to_string(),
            env: HashMap::new(),
            duplex_stdin: true,
        }
    }

    #[tokio::test]
    async fn a_spawned_process_streams_stdout_and_reports_its_exit_code() {
        let env = LocalEnvironment::new();
        let mut proc = env
            .spawn(spec("/bin/sh", &["-c", "echo hello; exit 3"], "/tmp"))
            .await
            .unwrap();

        let stdout = proc.take_stdout().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("hello"));
        assert_eq!(proc.wait().await.unwrap(), Some(3));
    }

    /// The property the whole v2 protocol rests on: a process can be written to
    /// after it has already produced output, because stdin was never closed.
    #[tokio::test]
    async fn stdin_stays_open_for_a_conversation_rather_than_one_write() {
        let env = LocalEnvironment::new();
        let mut proc = env
            .spawn(spec(
                "/bin/sh",
                &["-c", "while read line; do echo \"got:$line\"; done"],
                "/tmp",
            ))
            .await
            .unwrap();

        let mut stdin = proc.take_stdin().unwrap();
        let stdout = proc.take_stdout().unwrap();
        let mut lines = BufReader::new(stdout).lines();

        stdin.write_all(b"one\n").await.unwrap();
        stdin.flush().await.unwrap();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("got:one"));

        // The second write is the point: a one-shot pipe cannot do this, and
        // every native capability depends on it.
        stdin.write_all(b"two\n").await.unwrap();
        stdin.flush().await.unwrap();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("got:two"));

        drop(stdin);
        assert_eq!(proc.wait().await.unwrap(), Some(0));
    }

    #[tokio::test]
    async fn a_signal_reaches_a_child_the_process_spawned_not_only_the_wrapper() {
        let env = LocalEnvironment::new();
        // The outer sh execs sleep, so killing only the wrapper would leave the
        // sleep alive and `wait` would block past the assertion.
        let mut proc = env
            .spawn(spec("/bin/sh", &["-c", "exec sleep 30"], "/tmp"))
            .await
            .unwrap();

        proc.signaler().signal(Signal::Term).await.unwrap();
        // Killed by signal, so there is no exit code.
        assert_eq!(proc.wait().await.unwrap(), None);
    }

    /// The reason `signal` is not a method on the handle: a stop request arrives
    /// on a different task than the one draining output, so it must be able to
    /// signal while that task is parked in `wait()`.
    #[tokio::test]
    async fn a_signaller_can_stop_a_process_while_another_task_awaits_its_exit() {
        let env = LocalEnvironment::new();
        let mut proc = env
            .spawn(spec("/bin/sh", &["-c", "exec sleep 30"], "/tmp"))
            .await
            .unwrap();

        let signaler = proc.signaler();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            signaler.signal(Signal::Term).await
        });

        // Parked here with no access to the handle the stopper is using.
        assert_eq!(proc.wait().await.unwrap(), None);
        stopper.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn the_environment_passes_variables_through_to_the_process() {
        let env = LocalEnvironment::new();
        let mut spec = spec("/bin/sh", &["-c", "echo \"$SYLPH_TEST_VAR\""], "/tmp");
        spec.env.insert("SYLPH_TEST_VAR".into(), "present".into());

        let mut proc = env.spawn(spec).await.unwrap();
        let stdout = proc.take_stdout().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(lines.next_line().await.unwrap().as_deref(), Some("present"));
    }

    #[tokio::test]
    async fn signalling_an_already_exited_process_is_not_an_error() {
        // The caller's intent — "do not keep running" — is already satisfied, so
        // a race between exit and stop must not surface as a failure.
        let env = LocalEnvironment::new();
        let mut proc = env.spawn(spec("/bin/sh", &["-c", "exit 0"], "/tmp")).await.unwrap();
        let signaler = proc.signaler();
        proc.wait().await.unwrap();
        assert!(signaler.signal(Signal::Term).await.is_ok());
    }

    #[tokio::test]
    async fn a_missing_program_fails_at_spawn_with_the_path_in_the_message() {
        let env = LocalEnvironment::new();
        // Matched rather than `unwrap_err`: a live process handle cannot be
        // `Debug`, since its trait object would have to promise it for every
        // future environment.
        let message = match env.spawn(spec("/nonexistent/adapter.sh", &[], "/tmp")).await {
            Ok(_) => panic!("spawning a missing program must fail"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            message.contains("adapter.sh"),
            "the path belongs in the message: {message}"
        );
    }

    #[tokio::test]
    async fn is_dir_answers_for_the_host_filesystem() {
        let env = LocalEnvironment::new();
        assert!(env.is_dir("/tmp").await.unwrap());
        assert!(!env.is_dir("/tmp/definitely-not-here-9f3c").await.unwrap());
        // A file is not a directory, which is what session creation must reject.
        assert!(!env.is_dir("/etc/hosts").await.unwrap());
    }

    #[tokio::test]
    async fn the_local_environment_is_always_reachable_and_identifies_itself() {
        let env = LocalEnvironment::new();
        assert!(env.probe().await.available);
        let descriptor = env.descriptor();
        assert_eq!(descriptor.kind, "local");
        assert_eq!(descriptor.id, "local");
    }
}
