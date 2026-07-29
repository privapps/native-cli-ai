use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::time::{Duration, timeout};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Manages PTY sessions for sandboxed command execution.
pub struct PtyManager {
    workspace_root: std::path::PathBuf,
}

impl PtyManager {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Spawn a command in a new PTY, capture output, and return it.
    pub async fn exec(&self, command: &str, timeout_secs: u64) -> Result<PtyOutput, PtyError> {
        let mut cmd = shell_command(command);
        cmd.current_dir(&self.workspace_root)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| PtyError::SpawnFailed(e.to_string()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| PtyError::SpawnFailed("failed to capture stdout".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| PtyError::SpawnFailed("failed to capture stderr".into()))?;

        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();
        let result = timeout(Duration::from_secs(timeout_secs), async {
            let (stdout_result, stderr_result, status) = tokio::join!(
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
                child.wait(),
            );
            stdout_result.map_err(PtyError::Io)?;
            stderr_result.map_err(PtyError::Io)?;
            status.map_err(PtyError::Io)
        })
        .await;

        let status = match result {
            Ok(status) => status?,
            Err(_) => {
                if let Err(error) = terminate_child_tree(&mut child).await {
                    return Err(PtyError::TimeoutCleanupFailed {
                        timeout_secs,
                        error,
                    });
                }
                return Err(PtyError::Timeout(timeout_secs));
            }
        };

        let mut text = String::new();
        text.push_str(&String::from_utf8_lossy(&stdout_bytes));
        if !stderr_bytes.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&stderr_bytes));
        }

        Ok(PtyOutput {
            stdout: text,
            exit_code: status.code().unwrap_or(-1),
        })
    }

    /// Start the platform-native interactive shell in a portable PTY.
    ///
    /// The returned session owns the PTY input/output handles. Callers can
    /// write shell input with [`InteractivePtySession::write_input`] and read
    /// terminal output with [`InteractivePtySession::read_output`].
    pub fn spawn_interactive(&self) -> Result<InteractivePtySession, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize::default())
            .map_err(|e| PtyError::Interactive(e.to_string()))?;

        let mut command = interactive_shell_command();
        command.cwd(&self.workspace_root);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| PtyError::Interactive(e.to_string()))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Interactive(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Interactive(e.to_string()))?;

        Ok(InteractivePtySession {
            master: pair.master,
            reader: Arc::new(Mutex::new(reader)),
            writer,
            child,
        })
    }
}

/// A live platform-native interactive shell session.
pub struct InteractivePtySession {
    master: Box<dyn MasterPty + Send>,
    reader: Arc<Mutex<Box<dyn std::io::Read + Send>>>,
    writer: Box<dyn std::io::Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl InteractivePtySession {
    pub fn write_input(&mut self, input: &[u8]) -> Result<(), PtyError> {
        self.writer.write_all(input).map_err(PtyError::Io)?;
        self.writer.flush().map_err(PtyError::Io)
    }

    pub fn read_output(&mut self, output: &mut [u8]) -> Result<usize, PtyError> {
        self.reader
            .lock()
            .map_err(|_| PtyError::Interactive("PTY reader lock poisoned".into()))?
            .read(output)
            .map_err(PtyError::Io)
    }

    /// Read terminal output without blocking longer than `read_timeout`.
    pub async fn read_output_timeout(
        &self,
        output: &mut [u8],
        read_timeout: Duration,
    ) -> Result<usize, PtyError> {
        let reader = Arc::clone(&self.reader);
        let capacity = output.len();
        let read = timeout(
            read_timeout,
            tokio::task::spawn_blocking(move || {
                let mut buffer = vec![0_u8; capacity];
                let read = reader
                    .lock()
                    .map_err(|_| PtyError::Interactive("PTY reader lock poisoned".into()))?
                    .read(&mut buffer)
                    .map_err(PtyError::Io)?;
                Ok::<_, PtyError>((buffer, read))
            }),
        )
        .await
        .map_err(|_| PtyError::ReadTimeout(read_timeout))?
        .map_err(|error| PtyError::Interactive(error.to_string()))??;
        output[..read.1].copy_from_slice(&read.0[..read.1]);
        Ok(read.1)
    }

    pub fn try_wait(&mut self) -> Result<Option<portable_pty::ExitStatus>, PtyError> {
        self.child.try_wait().map_err(PtyError::Io)
    }

    pub fn wait(&mut self) -> Result<portable_pty::ExitStatus, PtyError> {
        self.child.wait().map_err(PtyError::Io)
    }

    pub fn terminate(&mut self) -> Result<(), PtyError> {
        self.child.kill().map_err(PtyError::Io)
    }

    pub fn resize(&self, size: PtySize) -> Result<(), PtyError> {
        self.master
            .resize(size)
            .map_err(|e| PtyError::Interactive(e.to_string()))
    }

    pub fn process_id(&self) -> Option<u32> {
        self.child.process_id()
    }
}

impl Drop for InteractivePtySession {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
    }
}

/// Build the platform's native command interpreter invocation.
///
/// `cmd.exe` is selected through `COMSPEC` when available so installations
/// that use a non-default command interpreter continue to work. `/D` prevents
/// AutoRun scripts from changing the command environment, and `/S /C` gives
/// `cmd.exe` its normal command-string behavior.
fn shell_command(command: &str) -> tokio::process::Command {
    #[cfg(unix)]
    {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-lc", command]);
        cmd
    }

    #[cfg(windows)]
    {
        let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
        let mut cmd = tokio::process::Command::new(shell);
        cmd.args(["/D", "/S", "/C", command]);
        cmd
    }
}

fn interactive_shell_command() -> CommandBuilder {
    #[cfg(unix)]
    {
        let mut command = CommandBuilder::new("sh");
        command.args(["-i"]);
        command
    }

    #[cfg(windows)]
    {
        let shell = std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into());
        CommandBuilder::new(shell)
    }
}

async fn terminate_child_tree(child: &mut tokio::process::Child) -> Result<(), String> {
    let pid = child
        .id()
        .ok_or_else(|| "timed-out process no longer has a PID".to_string())?;
    let mut tree_terminated = false;
    let mut cleanup_error = None;

    #[cfg(unix)]
    {
        if pid > i32::MAX as u32 {
            return Err(format!("timed-out process PID {pid} is out of range"));
        }
        let result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        if result == 0 {
            tree_terminated = true;
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                tree_terminated = true;
            } else {
                cleanup_error = Some(format!("failed to terminate process group {pid}: {error}"));
            }
        }
    }

    #[cfg(windows)]
    {
        let output = tokio::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output()
            .await
            .map_err(|e| format!("failed to run taskkill for process tree {pid}: {e}"))?;
        if !output.status.success() {
            let detail = command_output_detail(&output.stdout, &output.stderr);
            if !process_was_already_gone(&detail) {
                cleanup_error = Some(format!("taskkill failed for process tree {pid}: {detail}"));
            } else {
                tree_terminated = true;
            }
        } else {
            tree_terminated = true;
        }
    }

    if !tree_terminated
        && let Err(error) = child.kill().await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        cleanup_error = Some(format!(
            "failed to terminate timed-out process {pid}: {error}"
        ));
    }

    let wait_error = match child.wait().await {
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(format!("failed to reap timed-out process {pid}: {error}")),
    };
    if let Some(error) = cleanup_error.or(wait_error) {
        Err(error)
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    format!("{stderr} {stdout}").trim().to_string()
}

#[cfg(windows)]
fn process_was_already_gone(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("no such process")
        || detail.contains("not found")
        || detail.contains("no running instance of the task")
}

#[derive(Debug)]
pub struct PtyOutput {
    pub stdout: String,
    pub exit_code: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("Command timed out after {0}s")]
    Timeout(u64),
    #[error("Command timed out after {timeout_secs}s and cleanup failed: {error}")]
    TimeoutCleanupFailed { timeout_secs: u64, error: String },
    #[error("Spawn failed: {0}")]
    SpawnFailed(String),
    #[error("Interactive PTY failed: {0}")]
    Interactive(String),
    #[error("Interactive PTY read timed out after {0:?}")]
    ReadTimeout(Duration),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::PtyManager;
    use tokio::time::Duration;

    #[tokio::test]
    async fn executes_a_command_through_the_native_shell() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let output = PtyManager::new(workspace.path())
            .exec("echo nca-platform-shell", 5)
            .await
            .expect("native shell should start");

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("nca-platform-shell"));
    }

    #[tokio::test]
    async fn reports_spawn_failures_for_an_invalid_workspace() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let missing_workspace = workspace.path().join("missing");
        let error = PtyManager::new(missing_workspace)
            .exec("echo should-not-run", 5)
            .await
            .expect_err("invalid working directory should fail to spawn");

        assert!(error.to_string().contains("Spawn failed"));
    }

    #[tokio::test]
    async fn timeout_is_reported_and_child_is_reaped() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let error = PtyManager::new(workspace.path())
            .exec(sleep_command(), 1)
            .await
            .expect_err("long-running command should time out");

        assert!(matches!(error, super::PtyError::Timeout(1)));
    }

    #[tokio::test]
    async fn interactive_session_accepts_input_and_returns_output() {
        let workspace = tempfile::tempdir().expect("create workspace");
        let mut session = PtyManager::new(workspace.path())
            .spawn_interactive()
            .expect("interactive shell should start");
        let input = if cfg!(windows) {
            b"echo nca-interactive\r\nexit\r\n".as_slice()
        } else {
            b"echo nca-interactive\nexit\n".as_slice()
        };
        session
            .write_input(input)
            .expect("interactive shell should accept input");

        let mut buffer = [0_u8; 1024];
        let read = session
            .read_output_timeout(&mut buffer, Duration::from_secs(2))
            .await
            .expect("interactive shell should produce output");
        let output = String::from_utf8_lossy(&buffer[..read]);
        assert!(output.contains("nca-interactive"), "output was: {output:?}");
        session
            .terminate()
            .expect("interactive shell should terminate");
    }

    #[cfg(unix)]
    fn sleep_command() -> &'static str {
        "sleep 30"
    }

    #[cfg(windows)]
    fn sleep_command() -> &'static str {
        "ping 127.0.0.1 -n 31 >NUL"
    }
}
