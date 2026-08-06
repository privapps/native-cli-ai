use nca_common::session::{SessionSnapshot, SessionState};
use std::path::{Path, PathBuf};

/// Persists and loads session state to/from disk.
pub struct SessionStore {
    sessions_dir: PathBuf,
}

impl SessionStore {
    pub fn new(sessions_dir: impl AsRef<Path>) -> Self {
        Self {
            sessions_dir: sessions_dir.as_ref().to_path_buf(),
        }
    }

    pub fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Return the exact state path used by this store for a session.
    pub fn state_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.json"))
    }

    /// Return the exact event-log path used by this store for a session.
    pub fn events_path(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(format!("{session_id}.events.jsonl"))
    }

    pub async fn save(&self, session: &SessionState) -> Result<(), SessionStoreError> {
        let path = self.state_path(&session.meta.id);
        let json = serde_json::to_string_pretty(session)
            .map_err(|e| SessionStoreError::Serialize(e.to_string()))?;

        tokio::fs::create_dir_all(&self.sessions_dir)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        tokio::fs::write(&path, json)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        Ok(())
    }

    pub async fn load(&self, session_id: &str) -> Result<SessionState, SessionStoreError> {
        let path = self.state_path(session_id);
        let json = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        serde_json::from_str(&json).map_err(|e| SessionStoreError::Deserialize(e.to_string()))
    }

    pub async fn load_snapshot(
        &self,
        session_id: &str,
    ) -> Result<SessionSnapshot, SessionStoreError> {
        self.load(session_id)
            .await
            .map(|session| self.snapshot_with_paths(session_id, &session))
    }

    pub fn snapshot_with_paths(&self, session_id: &str, session: &SessionState) -> SessionSnapshot {
        let mut snapshot = session.snapshot();
        snapshot.state_path = Some(self.state_path(session_id));
        snapshot.events_path = Some(self.events_path(session_id));
        snapshot
    }

    pub async fn list(&self) -> Result<Vec<String>, SessionStoreError> {
        let mut ids = Vec::new();
        if !self.sessions_dir.exists() {
            return Ok(ids);
        }
        let mut entries = tokio::fs::read_dir(&self.sessions_dir)
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| SessionStoreError::Io(e.to_string()))?
        {
            if let Some(name) = entry.file_name().to_str()
                && let Some(id) = name.strip_suffix(".json")
            {
                ids.push(id.to_string());
            }
        }

        Ok(ids)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Serialization error: {0}")]
    Serialize(String),
    #[error("Deserialization error: {0}")]
    Deserialize(String),
}
