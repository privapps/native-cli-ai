use nca_common::config::IdleHookConfig;
use nca_common::event::{AgentEvent, BusyState};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::time::timeout;

const IDLE_HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// Startup-resolved idle hook owned by a supervised session.
#[derive(Clone)]
pub struct IdleHookRunner {
    command: Arc<str>,
    args: Arc<[String]>,
    busy: Arc<AtomicBool>,
    in_flight: Arc<AtomicBool>,
}

impl IdleHookRunner {
    pub fn from_config(config: &IdleHookConfig) -> Option<Self> {
        let command = config.command.as_deref()?.trim();
        if command.is_empty() {
            return None;
        }

        Some(Self {
            command: Arc::from(command.to_string()),
            args: Arc::from(config.args.clone()),
            busy: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Observe an authoritative runtime event and launch on a busy-to-idle transition.
    pub fn observe(&self, event: &AgentEvent) {
        let AgentEvent::BusyStateChanged { state } = event else {
            return;
        };

        if *state != BusyState::Idle {
            self.busy.store(true, Ordering::Release);
            return;
        }

        if !self.busy.swap(false, Ordering::AcqRel) {
            return;
        }

        if self.in_flight.swap(true, Ordering::AcqRel) {
            tracing::debug!(
                executable = %self.command,
                "idle hook skipped because an invocation is already running"
            );
            return;
        }

        let command = self.command.clone();
        let args = self.args.clone();
        let in_flight = self.in_flight.clone();
        tokio::spawn(async move {
            run_hook(command, args).await;
            in_flight.store(false, Ordering::Release);
        });
    }
}

async fn run_hook(command: Arc<str>, args: Arc<[String]>) {
    let started_at = Instant::now();
    tracing::debug!(executable = %command, "starting idle hook");

    let mut child = match Command::new(command.as_ref())
        .args(args.iter())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(
                executable = %command,
                error = %error,
                "idle hook could not be started"
            );
            return;
        }
    };

    let remaining = IDLE_HOOK_TIMEOUT.checked_sub(started_at.elapsed());
    match remaining {
        Some(remaining) => match timeout(remaining, child.wait()).await {
            Ok(Ok(status)) if status.success() => {
                tracing::debug!(executable = %command, "idle hook completed successfully");
            }
            Ok(Ok(status)) => {
                tracing::warn!(
                    executable = %command,
                    exit_code = ?status.code(),
                    "idle hook exited unsuccessfully"
                );
            }
            Ok(Err(error)) => {
                tracing::warn!(
                    executable = %command,
                    error = %error,
                    "idle hook failed while running"
                );
            }
            Err(_) => log_timeout(&command, &mut child).await,
        },
        None => log_timeout(&command, &mut child).await,
    }
}

async fn log_timeout(command: &str, child: &mut Child) {
    let termination_error = terminate_child(child).await.err();
    match termination_error {
        Some(error) => tracing::warn!(
            executable = %command,
            error = %error,
            "idle hook timed out and termination was incomplete"
        ),
        None => tracing::warn!(
            executable = %command,
            "idle hook timed out and was terminated"
        ),
    }
}

async fn terminate_child(child: &mut Child) -> Result<(), String> {
    let mut error = None;

    if let Err(kill_error) = child.kill().await
        && kill_error.kind() != std::io::ErrorKind::NotFound
    {
        error = Some(format!("failed to terminate idle hook: {kill_error}"));
    }

    if let Err(wait_error) = child.wait().await
        && wait_error.kind() != std::io::ErrorKind::NotFound
    {
        error = Some(format!("failed to reap terminated idle hook: {wait_error}"));
    }

    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_only_commands_are_disabled() {
        let config = IdleHookConfig {
            command: Some("  \t".into()),
            args: vec!["ignored".into()],
        };
        assert!(IdleHookRunner::from_config(&config).is_none());
    }
}

#[cfg(test)]
mod transition_tests {
    use super::*;

    #[test]
    fn runner_starts_without_a_busy_predecessor() {
        let config = IdleHookConfig {
            command: Some("does-not-run".into()),
            args: Vec::new(),
        };
        let runner = IdleHookRunner::from_config(&config).expect("runner");
        runner.observe(&AgentEvent::BusyStateChanged {
            state: BusyState::Idle,
        });
        assert!(!runner.busy.load(Ordering::SeqCst));
        assert!(!runner.in_flight.load(Ordering::SeqCst));
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn timeout_terminates_the_child_and_releases_the_in_flight_guard() {
        let workspace = tempfile::tempdir().expect("workspace");
        let script = workspace.path().join("timeout-hook");
        let count = workspace.path().join("count");
        let output = workspace.path().join("output");
        fs::write(
            &script,
            "#!/bin/sh\ncount_file=$1\nout=$2\ncount=0\n[ -f \"$count_file\" ] && count=$(cat \"$count_file\")\ncount=$((count + 1))\nprintf '%s' \"$count\" > \"$count_file\"\nif [ \"$count\" -eq 1 ]; then exec sleep 30; fi\nprintf recovered > \"$out\"\n",
        )
        .expect("write hook");
        let mut permissions = fs::metadata(&script).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make hook executable");

        let runner = IdleHookRunner::from_config(&IdleHookConfig {
            command: Some(script.to_string_lossy().into_owned()),
            args: vec![
                count.to_string_lossy().into_owned(),
                output.to_string_lossy().into_owned(),
            ],
        })
        .expect("runner");

        runner.observe(&AgentEvent::BusyStateChanged {
            state: BusyState::Thinking,
        });
        runner.observe(&AgentEvent::BusyStateChanged {
            state: BusyState::Idle,
        });

        tokio::time::sleep(IDLE_HOOK_TIMEOUT + Duration::from_millis(250)).await;
        assert_eq!(fs::read_to_string(&count).expect("first invocation"), "1");
        assert!(!output.exists(), "timed-out invocation must not complete");
        assert!(!runner.in_flight.load(Ordering::Acquire));

        runner.observe(&AgentEvent::BusyStateChanged {
            state: BusyState::Thinking,
        });
        runner.observe(&AgentEvent::BusyStateChanged {
            state: BusyState::Idle,
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if output.exists() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a later transition should recover after timeout");
        assert_eq!(
            fs::read_to_string(&output).expect("recovery output"),
            "recovered"
        );
    }
}
