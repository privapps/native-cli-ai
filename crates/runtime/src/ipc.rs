use nca_common::event::{AgentCommand, EventEnvelope};
use std::path::Path;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

#[cfg(windows)]
type IpcListener = tokio::net::TcpListener;
#[cfg(unix)]
type IpcListener = tokio::net::UnixListener;
#[cfg(windows)]
type IpcStream = tokio::net::TcpStream;
#[cfg(unix)]
type IpcStream = tokio::net::UnixStream;

#[cfg(windows)]
const WINDOWS_IPC_BIND_ATTEMPTS: usize = 8;

/// IPC server that broadcasts AgentEvents and receives AgentCommands
/// over a Unix domain socket or Windows loopback TCP.
pub struct IpcServer {
    socket_path: PathBuf,
    #[cfg(windows)]
    windows_listener: Option<std::net::TcpListener>,
    #[cfg(windows)]
    windows_bind_error: Option<String>,
}

impl IpcServer {
    pub fn new(session_id: &str) -> Self {
        #[cfg(unix)]
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        #[cfg(unix)]
        let socket_path = runtime_dir.join("nca").join(format!("{session_id}.sock"));
        #[cfg(windows)]
        let (socket_path, windows_listener, windows_bind_error) = match allocate_windows_listener()
        {
            Ok((listener, endpoint)) => (endpoint, Some(listener), None),
            Err(error) => (PathBuf::from("127.0.0.1:0"), None, Some(error)),
        };
        #[cfg(windows)]
        let _ = session_id;
        Self {
            socket_path,
            #[cfg(windows)]
            windows_listener,
            #[cfg(windows)]
            windows_bind_error,
        }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone()
    }

    /// Start listening for client connections.
    pub async fn start(&self) -> Result<IpcHandle, IpcError> {
        #[cfg(unix)]
        if let Some(parent) = self.socket_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|err| {
                IpcError::ConnectionFailed(format!(
                    "failed to prepare IPC directory {} for endpoint {}: {err}",
                    parent.display(),
                    self.socket_path.display()
                ))
            })?;
        }
        #[cfg(unix)]
        if self.socket_path.exists() {
            let _ = tokio::fs::remove_file(&self.socket_path).await;
        }

        #[cfg(unix)]
        let listener = bind_listener(&self.socket_path).await?;
        #[cfg(windows)]
        let listener = self.windows_listener()?;
        let (event_tx, _) = broadcast::channel::<String>(256);
        let accept_event_tx = event_tx.clone();
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let socket_path = self.socket_path.clone();

        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let event_rx = accept_event_tx.subscribe();
                let command_tx = command_tx.clone();
                tokio::spawn(handle_connection(stream, event_rx, command_tx));
            }
            cleanup_endpoint(&socket_path).await;
        });

        Ok(IpcHandle {
            socket_path: self.socket_path.clone(),
            event_tx,
            command_rx,
        })
    }
}

pub struct IpcHandle {
    socket_path: PathBuf,
    event_tx: broadcast::Sender<String>,
    command_rx: mpsc::UnboundedReceiver<AgentCommand>,
}

impl IpcHandle {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub async fn broadcast(&self, event: &EventEnvelope) -> Result<(), IpcError> {
        let line = serde_json::to_string(event)
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
        let _ = self.event_tx.send(line);
        Ok(())
    }

    pub async fn recv_command(&mut self) -> Option<AgentCommand> {
        self.command_rx.recv().await
    }

    /// Split into parts for separate tasks: event broadcast and command receiver.
    pub fn into_parts(
        self,
    ) -> (
        broadcast::Sender<String>,
        mpsc::UnboundedReceiver<AgentCommand>,
    ) {
        (self.event_tx, self.command_rx)
    }
}

/// IPC client for connecting to a running session socket (events, approvals, shutdown).
pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn connect(&self) -> Result<mpsc::Receiver<EventEnvelope>, IpcError> {
        let stream = connect_stream(&self.socket_path).await?;
        let (tx, rx) = mpsc::channel(128);
        tokio::spawn(async move {
            let reader = BufReader::new(stream);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(event) = serde_json::from_str::<EventEnvelope>(&line)
                    && tx.send(event).await.is_err()
                {
                    break;
                }
            }
        });
        Ok(rx)
    }

    pub async fn send_command(&self, cmd: &AgentCommand) -> Result<(), IpcError> {
        let mut stream = connect_stream(&self.socket_path).await?;
        let line = serde_json::to_string(cmd)
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
        stream
            .write_all(line.as_bytes())
            .await
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
        stream
            .write_all(b"\n")
            .await
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}

#[cfg(unix)]
async fn bind_listener(endpoint: &Path) -> Result<IpcListener, IpcError> {
    IpcListener::bind(endpoint).map_err(|err| {
        IpcError::ConnectionFailed(format!(
            "failed to bind IPC endpoint {}: {err}",
            endpoint.display()
        ))
    })
}

#[cfg(windows)]
impl IpcServer {
    fn windows_listener(&self) -> Result<IpcListener, IpcError> {
        let listener = self
            .windows_listener
            .as_ref()
            .ok_or_else(|| {
                IpcError::ConnectionFailed(
                    self.windows_bind_error
                        .clone()
                        .unwrap_or_else(|| "Windows IPC endpoint was not allocated".to_string()),
                )
            })?
            .try_clone()
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;

        tokio::net::TcpListener::from_std(listener)
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))
    }
}

#[cfg(unix)]
async fn connect_stream(endpoint: &Path) -> Result<IpcStream, IpcError> {
    IpcStream::connect(endpoint)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))
}

#[cfg(windows)]
async fn connect_stream(endpoint: &Path) -> Result<IpcStream, IpcError> {
    IpcStream::connect(endpoint.to_string_lossy().as_ref())
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))
}

#[cfg(unix)]
async fn cleanup_endpoint(endpoint: &Path) {
    let _ = tokio::fs::remove_file(endpoint).await;
}

#[cfg(windows)]
async fn cleanup_endpoint(_endpoint: &Path) {}

#[cfg(windows)]
fn allocate_windows_listener() -> Result<(std::net::TcpListener, PathBuf), String> {
    allocate_windows_listener_with(|| std::net::TcpListener::bind(("127.0.0.1", 0)))
}

#[cfg(windows)]
fn allocate_windows_listener_with<F>(
    mut bind: F,
) -> Result<(std::net::TcpListener, PathBuf), String>
where
    F: FnMut() -> std::io::Result<std::net::TcpListener>,
{
    let mut last_error = None;

    for _ in 0..WINDOWS_IPC_BIND_ATTEMPTS {
        match bind() {
            Ok(listener) => {
                listener
                    .set_nonblocking(true)
                    .map_err(|err| format!("failed to configure Windows IPC listener: {err}"))?;
                let endpoint = listener
                    .local_addr()
                    .map_err(|err| format!("failed to inspect Windows IPC listener: {err}"))?;
                return Ok((listener, PathBuf::from(endpoint.to_string())));
            }
            Err(error) => last_error = Some(error),
        }
    }

    let error = last_error.expect("Windows IPC bind attempts must be non-empty");
    Err(format!(
        "failed to allocate Windows IPC loopback endpoint after {WINDOWS_IPC_BIND_ATTEMPTS} attempts: {error}"
    ))
}

async fn handle_connection(
    stream: IpcStream,
    mut event_rx: broadcast::Receiver<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let read_task = tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(command) = serde_json::from_str::<AgentCommand>(&line) {
                let _ = command_tx.send(command);
            }
        }
    });

    let write_task = tokio::spawn(async move {
        while let Ok(line) = event_rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let _ = tokio::join!(read_task, write_task);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ipc_round_trips_newline_delimited_commands() {
        let server = IpcServer::new("ipc-framing-test");
        let endpoint = server.socket_path();
        let mut handle = server.start().await.expect("IPC server should bind");
        assert_eq!(endpoint, handle.socket_path().clone());

        IpcClient::new(endpoint)
            .send_command(&AgentCommand::Shutdown)
            .await
            .expect("IPC client should send a command");

        let command =
            tokio::time::timeout(std::time::Duration::from_secs(1), handle.recv_command())
                .await
                .expect("IPC command should arrive promptly");
        assert!(matches!(command, Some(AgentCommand::Shutdown)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_bind_failures_report_endpoint_context() {
        let temp = tempfile::tempdir().expect("temporary IPC directory should be created");
        let endpoint = temp.path().join("occupied.sock");
        std::fs::create_dir(&endpoint).expect("endpoint collision fixture should be created");

        let result = bind_listener(&endpoint).await;
        let error = result.unwrap_err();
        assert!(error.to_string().contains(&endpoint.display().to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_servers_reserve_distinct_ephemeral_loopback_endpoints() {
        let first = IpcServer::new("session-123");
        let second = IpcServer::new("session-123");

        assert!(
            first
                .socket_path()
                .to_string_lossy()
                .starts_with("127.0.0.1:")
        );
        assert!(
            second
                .socket_path()
                .to_string_lossy()
                .starts_with("127.0.0.1:")
        );
        assert_ne!(first.socket_path(), second.socket_path());
    }

    #[cfg(windows)]
    #[test]
    fn windows_endpoint_allocation_retries_bind_collisions() {
        let mut attempts = 0;
        let (listener, endpoint) = allocate_windows_listener_with(|| {
            attempts += 1;
            if attempts == 1 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "simulated occupied endpoint",
                ))
            } else {
                std::net::TcpListener::bind(("127.0.0.1", 0))
            }
        })
        .expect("allocation should retry after a collision");

        assert_eq!(attempts, 2);
        assert!(endpoint.to_string_lossy().starts_with("127.0.0.1:"));
        drop(listener);
    }

    #[cfg(windows)]
    #[test]
    fn windows_endpoint_allocation_reports_retry_exhaustion() {
        let result = allocate_windows_listener_with(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "simulated occupied endpoint",
            ))
        });

        let error = result.unwrap_err();
        assert!(error.contains("after 8 attempts"));
        assert!(error.contains("simulated occupied endpoint"));
    }
}
