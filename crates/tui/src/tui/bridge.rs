//! Event fanout: session log, IPC, and TUI state (no stdout streaming).

use crate::ipc_pending::{ApprovalPendingMap, QuestionPendingMap};
use crate::tui::state::TuiSessionState;
use nca_common::event::{AgentEvent, EventEnvelope};
use nca_runtime::idle_hook::IdleHookRunner;
use nca_runtime::ipc::IpcHandle;
use nca_runtime::supervisor;
use std::sync::{Arc, Mutex};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

struct IpcFanout {
    tx: tokio::sync::broadcast::Sender<String>,
}

/// Disk + IPC + TUI state; starts IPC command consumer when needed.
pub fn spawn_tui_bridge(
    rx: tokio::sync::mpsc::Receiver<AgentEvent>,
    log_path: std::path::PathBuf,
    ipc_handle: Option<IpcHandle>,
    approval_pending: Option<ApprovalPendingMap>,
    question_pending: Option<QuestionPendingMap>,
    state: Arc<Mutex<TuiSessionState>>,
    version_tx: Option<tokio::sync::watch::Sender<u64>>,
) -> tokio::task::JoinHandle<()> {
    spawn_tui_bridge_with_idle_hook(
        rx,
        log_path,
        ipc_handle,
        approval_pending,
        question_pending,
        state,
        version_tx,
        None,
    )
}

/// Disk + IPC + TUI state with the startup-resolved idle-hook observer.
#[allow(clippy::too_many_arguments)]
pub fn spawn_tui_bridge_with_idle_hook(
    mut rx: tokio::sync::mpsc::Receiver<AgentEvent>,
    log_path: std::path::PathBuf,
    ipc_handle: Option<IpcHandle>,
    approval_pending: Option<ApprovalPendingMap>,
    question_pending: Option<QuestionPendingMap>,
    state: Arc<Mutex<TuiSessionState>>,
    version_tx: Option<tokio::sync::watch::Sender<u64>>,
    idle_hook: Option<IdleHookRunner>,
) -> tokio::task::JoinHandle<()> {
    let (event_tx_ipc, command_rx) = match ipc_handle {
        Some(h) => {
            let (etx, crx) = h.into_parts();
            (Some(etx), Some(crx))
        }
        None => (None, None),
    };

    if let Some(crx) = command_rx {
        supervisor::spawn_command_consumer(crx, approval_pending, question_pending, None);
    }

    let ipc = event_tx_ipc.map(|tx| IpcFanout { tx });

    tokio::spawn(async move {
        let mut log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
            .ok();

        let mut event_id: u64 = 0;
        while let Some(event) = rx.recv().await {
            event_id += 1;
            if let Some(ref runner) = idle_hook {
                runner.observe(&event);
            }
            let envelope = EventEnvelope::new(event_id, event.clone());

            if let Some(ref fan) = ipc {
                let line = serde_json::to_string(&envelope).unwrap_or_default();
                let _ = fan.tx.send(line);
            }

            if let Some(file) = log_file.as_mut()
                && let Ok(line) = serde_json::to_string(&envelope)
            {
                let _ = file.write_all(line.as_bytes()).await;
                let _ = file.write_all(b"\n").await;
            }

            if let Ok(mut g) = state.lock() {
                let before = g.state_version;
                g.apply_event(&event);
                if g.state_version != before
                    && let Some(tx) = &version_tx
                {
                    let _ = tx.send(g.state_version);
                }
            }
        }
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use nca_common::config::IdleHookConfig;
    use nca_common::event::BusyState;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn bridge_observes_busy_to_idle_events_for_idle_hook() {
        let workspace = tempfile::tempdir().expect("workspace");
        let script = workspace.path().join("idle-hook");
        let output = workspace.path().join("observed");
        let output_literal = output.to_string_lossy().replace('\'', "'\\''");
        fs::write(
            &script,
            format!("#!/bin/sh\nprintf observed >> '{output_literal}'\n"),
        )
        .expect("write hook");
        let mut permissions = fs::metadata(&script).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make hook executable");

        let runner = IdleHookRunner::from_config(&IdleHookConfig {
            command: Some(script.to_string_lossy().into_owned()),
            args: Vec::new(),
        })
        .expect("idle hook runner");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let state = Arc::new(Mutex::new(TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "default".into(),
            workspace.path().to_path_buf(),
        )));
        let log_path = workspace.path().join("events.jsonl");
        let bridge = spawn_tui_bridge_with_idle_hook(
            rx,
            log_path,
            None,
            None,
            None,
            state,
            None,
            Some(runner),
        );

        for state in [
            BusyState::Thinking,
            BusyState::Streaming,
            BusyState::Idle,
            BusyState::Idle,
        ] {
            tx.send(AgentEvent::BusyStateChanged { state })
                .await
                .expect("send event");
        }
        drop(tx);
        bridge.await.expect("bridge exits");

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if output.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("bridge should start the idle hook");
        assert_eq!(fs::read_to_string(output).expect("hook output"), "observed");
    }
}
