//! Git integration for autonomous research
//!
//! Handles branch creation, commits, and revert operations for experiments.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const EXPERIMENT_METADATA_FILE: &str = ".nca-autoresearch-experiment.json";

// Git hooks export repository-local variables such as GIT_INDEX_FILE. Those
// variables describe the repository that invoked the hook, not necessarily
// the repository/worktree GitManager is operating on. In particular, a
// linked worktree has a `.git` file rather than a `.git` directory, so an
// inherited relative GIT_INDEX_FILE can make Git report `.git/index` as
// "Not a directory".
const INHERITED_GIT_REPOSITORY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_NAMESPACE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_GRAFT_FILE",
    "GIT_SHALLOW_FILE",
    "GIT_IMPLICIT_WORK_TREE",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExperimentMetadata {
    version: u8,
    id: String,
    repository: PathBuf,
    base_commit: String,
}

/// An experiment worktree created from a stable baseline commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExperimentWorkspace {
    pub id: String,
    pub path: PathBuf,
    pub base_commit: String,
}

/// The explicit disposition requested after an experiment finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeDecision {
    Keep,
    Discard,
    Failure,
}

/// What happened to the isolated worktree during finalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeDisposition {
    Retained,
    Removed,
}

/// Git manager for autonomous research workflows
pub struct GitManager {
    repo_path: PathBuf,
}

impl GitManager {
    /// Create a new Git manager for the given repository
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }

    /// Create a detached, collision-safe worktree for one experiment.
    ///
    /// The worktree starts at `HEAD`, not at the caller's dirty index or
    /// working tree. A pre-existing experiment id is always an error; an
    /// interrupted experiment must be explicitly finalized or recovered and
    /// is never silently reused.
    pub async fn create_isolated_worktree(
        &self,
        experiments_root: &Path,
        experiment_id: &str,
    ) -> Result<ExperimentWorkspace> {
        validate_experiment_id(experiment_id)?;
        let path = experiments_root.join(experiment_id);
        if path.exists() {
            anyhow::bail!(
                "experiment workspace already exists; refusing to reuse interrupted state: {}",
                path.display()
            );
        }

        tokio::fs::create_dir_all(experiments_root)
            .await
            .with_context(|| {
                format!(
                    "Failed to create experiment workspace root: {}",
                    experiments_root.display()
                )
            })?;
        let base_commit = self.current_commit_full().await?;
        let path_string = path.to_string_lossy().into_owned();
        self.run(&[
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            &path_string,
            &base_commit,
        ])
        .await
        .with_context(|| format!("Failed to create isolated experiment {experiment_id}"))?;

        if let Err(error) = run_git_at(&path, &["reset", "--hard", &base_commit]).await {
            let _ = self.remove_worktree(&path, true).await;
            return Err(error).with_context(|| {
                format!("Failed to materialize isolated experiment {experiment_id}")
            });
        }

        if let Err(error) = self.write_experiment_metadata(&path, experiment_id, &base_commit) {
            let _ = self.remove_worktree(&path, true).await;
            return Err(error).with_context(|| {
                format!("Failed to persist isolated experiment {experiment_id} identity")
            });
        }

        Ok(ExperimentWorkspace {
            id: experiment_id.to_string(),
            path,
            base_commit,
        })
    }

    /// Explicitly recover an interrupted experiment worktree.
    ///
    /// Recovery requires all durable identity checks to agree: the path must
    /// still be a registered worktree owned by this repository, its marker
    /// must name the requested experiment, and its HEAD must still be the
    /// recorded baseline. A missing or mismatched marker is refusal, never
    /// an invitation to reuse an unknown directory.
    pub async fn recover_isolated_worktree(
        &self,
        experiments_root: &Path,
        experiment_id: &str,
    ) -> Result<ExperimentWorkspace> {
        validate_experiment_id(experiment_id)?;
        let path = experiments_root.join(experiment_id);
        if !path.is_dir() {
            anyhow::bail!(
                "cannot recover interrupted experiment {experiment_id}: workspace is missing"
            );
        }

        let canonical_path = std::fs::canonicalize(&path)
            .with_context(|| format!("canonicalize experiment workspace {}", path.display()))?;
        let registered = self
            .list_worktrees()
            .await?
            .into_iter()
            .find(|candidate| {
                std::fs::canonicalize(&candidate.path).ok().as_ref() == Some(&canonical_path)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "refusing to recover unregistered experiment workspace: {}",
                    path.display()
                )
            })?;

        let metadata_path = path.join(EXPERIMENT_METADATA_FILE);
        let metadata: ExperimentMetadata =
            serde_json::from_str(&std::fs::read_to_string(&metadata_path).with_context(|| {
                format!(
                    "read experiment identity marker {}",
                    metadata_path.display()
                )
            })?)
            .with_context(|| {
                format!(
                    "parse experiment identity marker {}",
                    metadata_path.display()
                )
            })?;
        let repository =
            std::fs::canonicalize(&self.repo_path).unwrap_or_else(|_| self.repo_path.clone());
        if metadata.version != 1
            || metadata.id != experiment_id
            || metadata.repository != repository
        {
            anyhow::bail!(
                "refusing to recover experiment {experiment_id}: identity marker does not match this repository"
            );
        }

        let workspace = ExperimentWorkspace {
            id: experiment_id.to_string(),
            path: path.clone(),
            base_commit: metadata.base_commit.clone(),
        };
        let head = self.worktree_head(&workspace).await?;
        if head != metadata.base_commit || registered.head != metadata.base_commit {
            anyhow::bail!(
                "refusing to recover experiment {experiment_id}: worktree HEAD no longer matches its recorded baseline"
            );
        }

        Ok(workspace)
    }

    fn write_experiment_metadata(
        &self,
        path: &Path,
        experiment_id: &str,
        base_commit: &str,
    ) -> Result<()> {
        let repository =
            std::fs::canonicalize(&self.repo_path).unwrap_or_else(|_| self.repo_path.clone());
        let metadata = ExperimentMetadata {
            version: 1,
            id: experiment_id.to_string(),
            repository,
            base_commit: base_commit.to_string(),
        };
        let marker = path.join(EXPERIMENT_METADATA_FILE);
        let temporary = path.join(format!("{EXPERIMENT_METADATA_FILE}.tmp"));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&metadata)?)?;
        std::fs::rename(&temporary, &marker)
            .with_context(|| format!("install experiment identity marker {}", marker.display()))?;
        Ok(())
    }

    /// Return the current HEAD of an experiment worktree.
    pub async fn worktree_head(&self, workspace: &ExperimentWorkspace) -> Result<String> {
        run_git_at(&workspace.path, &["rev-parse", "HEAD"]).await
    }

    /// List tracked and untracked files changed by an experiment.
    pub async fn workspace_changed_files(
        &self,
        workspace: &ExperimentWorkspace,
    ) -> Result<Vec<PathBuf>> {
        let tracked = run_git_at(&workspace.path, &["diff", "--name-only", "HEAD"]).await?;
        let untracked = run_git_at(
            &workspace.path,
            &["ls-files", "--others", "--exclude-standard"],
        )
        .await?;

        let mut files = tracked
            .lines()
            .chain(untracked.lines())
            .filter(|line| !line.is_empty())
            .filter(|line| *line != EXPERIMENT_METADATA_FILE)
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        Ok(files)
    }

    /// Ensure an experiment changed only explicitly permitted relative paths.
    pub async fn validate_permitted_files(
        &self,
        workspace: &ExperimentWorkspace,
        permitted_files: &[PathBuf],
    ) -> Result<Vec<PathBuf>> {
        let changed = self.workspace_changed_files(workspace).await?;
        let unexpected = changed
            .iter()
            .filter(|path| !permitted_files.iter().any(|allowed| allowed == *path))
            .cloned()
            .collect::<Vec<_>>();
        if !unexpected.is_empty() {
            anyhow::bail!(
                "experiment changed files outside the permitted set: {}",
                unexpected
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(changed)
    }

    /// Keep an improved worktree or remove a regressed/failed one.
    ///
    /// Keeping never merges, stages, commits, or modifies the primary
    /// repository. Discarding removes only the explicitly created worktree.
    pub async fn finalize_experiment(
        &self,
        workspace: &ExperimentWorkspace,
        decision: WorktreeDecision,
    ) -> Result<WorktreeDisposition> {
        let repo =
            std::fs::canonicalize(&self.repo_path).unwrap_or_else(|_| self.repo_path.clone());
        let path =
            std::fs::canonicalize(&workspace.path).unwrap_or_else(|_| workspace.path.clone());
        if path == repo {
            anyhow::bail!("refusing to finalize the primary repository as an experiment worktree");
        }

        let registered = self.list_worktrees().await?.into_iter().any(|candidate| {
            std::fs::canonicalize(candidate.path).ok()
                == std::fs::canonicalize(&workspace.path).ok()
        });
        if !registered {
            anyhow::bail!(
                "refusing to finalize an unregistered experiment worktree: {}",
                workspace.path.display()
            );
        }

        match decision {
            WorktreeDecision::Keep => Ok(WorktreeDisposition::Retained),
            WorktreeDecision::Discard | WorktreeDecision::Failure => {
                self.remove_worktree(&workspace.path, true).await?;
                Ok(WorktreeDisposition::Removed)
            }
        }
    }

    /// Run a git command and return the output
    async fn run(&self, args: &[&str]) -> Result<String> {
        let output = git_command_at(&self.repo_path, args)
            .output()
            .await
            .context("Failed to run git command")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {}", stderr);
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get the current branch name
    pub async fn current_branch(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "--abbrev-ref", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Get the current commit hash (short form)
    pub async fn current_commit(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "--short", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Get the full current commit hash
    pub async fn current_commit_full(&self) -> Result<String> {
        let output = self.run(&["rev-parse", "HEAD"]).await?;
        Ok(output.trim().to_string())
    }

    /// Create a new branch
    pub async fn create_branch(&self, name: &str) -> Result<()> {
        self.run(&["checkout", "-b", name]).await?;
        tracing::info!("Created branch: {}", name);
        Ok(())
    }

    /// Checkout an existing branch
    pub async fn checkout(&self, branch: &str) -> Result<()> {
        self.run(&["checkout", branch]).await?;
        Ok(())
    }

    /// Switch to a branch (newer git)
    pub async fn switch(&self, branch: &str) -> Result<()> {
        self.run(&["switch", branch]).await?;
        Ok(())
    }

    /// Create and switch to a new branch
    pub async fn create_and_switch(&self, branch: &str) -> Result<()> {
        // Try switch first (newer git), fall back to checkout
        if self.run(&["switch", "-c", branch]).await.is_err() {
            self.run(&["checkout", "-b", branch]).await?;
        }
        tracing::info!("Created and switched to branch: {}", branch);
        Ok(())
    }

    /// Commit staged changes with a message
    pub async fn commit(&self, message: &str) -> Result<String> {
        // Stage all changes
        self.run(&["add", "-A"]).await?;

        // Check if there are changes to commit
        let status = self.run(&["status", "--porcelain"]).await?;
        if status.trim().is_empty() {
            anyhow::bail!("No changes to commit");
        }

        // Commit
        self.run(&["commit", "-m", message]).await?;

        // Return the new commit hash
        let commit = self.current_commit().await?;
        tracing::info!("Committed: {} - {}", commit, message);
        Ok(commit)
    }

    /// Get the commit message for a given commit
    pub async fn commit_message(&self, commit: &str) -> Result<String> {
        let output = self.run(&["log", "-1", "--format=%B", commit]).await?;
        Ok(output.trim().to_string())
    }

    /// Reset to a specific commit (keeping changes)
    pub async fn reset_soft(&self, commit: &str) -> Result<()> {
        self.run(&["reset", "--soft", commit]).await?;
        tracing::info!("Soft reset to: {}", commit);
        Ok(())
    }

    /// Reset to a specific commit (discarding changes)
    pub async fn reset_hard(&self, commit: &str) -> Result<()> {
        self.run(&["reset", "--hard", commit]).await?;
        tracing::info!("Hard reset to: {}", commit);
        Ok(())
    }

    /// Get the diff between two commits
    pub async fn diff(&self, from: &str, to: &str) -> Result<String> {
        let output = self.run(&["diff", from, to]).await?;
        Ok(output)
    }

    /// Show files changed in a commit
    pub async fn changed_files(&self, commit: &str) -> Result<Vec<String>> {
        let output = self
            .run(&["diff-tree", "--no-commit-id", "--name-only", "-r", commit])
            .await?;
        Ok(output
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }

    /// Check if there are uncommitted changes
    pub async fn is_dirty(&self) -> Result<bool> {
        let status = self.run(&["status", "--porcelain"]).await?;
        Ok(!status.trim().is_empty())
    }

    /// Discard local changes to a file
    pub async fn checkout_file(&self, file: &Path) -> Result<()> {
        let file_str = file.to_string_lossy();
        self.run(&["checkout", "--", &file_str]).await?;
        Ok(())
    }

    /// Get the log of commits
    pub async fn log(&self, count: usize) -> Result<Vec<CommitInfo>> {
        let output = self
            .run(&["log", &format!("-{}", count), "--format=%H|%s|%ci"])
            .await?;

        let commits = output
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() >= 3 {
                    Some(CommitInfo {
                        hash: parts[0].to_string(),
                        message: parts[1].to_string(),
                        date: parts[2].to_string(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(commits)
    }

    /// Create a worktree for parallel experiments
    pub async fn create_worktree(
        &self,
        branch: &str,
        path: &Path,
        start_commit: Option<&str>,
    ) -> Result<()> {
        let mut args = vec!["worktree", "add", "-b", branch, path.to_str().unwrap_or("")];

        if let Some(commit) = start_commit {
            args.push(commit);
        }

        self.run(&args).await?;
        tracing::info!(
            "Created worktree at: {} on branch: {}",
            path.display(),
            branch
        );
        Ok(())
    }

    /// List worktrees
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.run(&["worktree", "list", "--porcelain"]).await?;

        let mut worktrees = Vec::new();
        let mut current: Option<WorktreeInfo> = None;

        for line in output.lines() {
            if line.starts_with("worktree ") {
                if let Some(w) = current.take() {
                    worktrees.push(w);
                }
                let path = line.trim_start_matches("worktree ");
                current = Some(WorktreeInfo {
                    path: PathBuf::from(path),
                    branch: String::new(),
                    head: String::new(),
                });
            } else if let Some(ref mut w) = current {
                if line.starts_with("branch ") {
                    w.branch = line.trim_start_matches("branch ").to_string();
                } else if line.starts_with("HEAD ") {
                    w.head = line.trim_start_matches("HEAD ").to_string();
                }
            }
        }

        if let Some(w) = current {
            worktrees.push(w);
        }

        Ok(worktrees)
    }

    /// Remove a worktree
    pub async fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push(path.to_str().unwrap_or(""));

        self.run(&args).await?;
        tracing::info!("Removed worktree: {}", path.display());
        Ok(())
    }

    /// Stash changes
    pub async fn stash(&self, message: Option<&str>) -> Result<()> {
        match message {
            Some(msg) => {
                self.run(&["stash", "push", "-m", msg]).await?;
            }
            None => {
                self.run(&["stash"]).await?;
            }
        }
        Ok(())
    }

    /// Apply stashed changes
    pub async fn stash_pop(&self) -> Result<()> {
        self.run(&["stash", "pop"]).await?;
        Ok(())
    }
}

fn validate_experiment_id(experiment_id: &str) -> Result<()> {
    if experiment_id.is_empty()
        || experiment_id == "."
        || experiment_id == ".."
        || !experiment_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        anyhow::bail!("experiment id must be a non-empty path-safe identifier: {experiment_id:?}");
    }
    Ok(())
}

async fn run_git_at(path: &Path, args: &[&str]) -> Result<String> {
    let output = git_command_at(path, args)
        .output()
        .await
        .context("Failed to run git command in experiment worktree")?;
    if !output.status.success() {
        anyhow::bail!(
            "Git command failed in {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_command_at(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(path).args(args);
    for variable in INHERITED_GIT_REPOSITORY_ENV {
        command.env_remove(variable);
    }
    command
}

/// Information about a git commit
#[derive(Debug, Clone)]
pub struct CommitInfo {
    pub hash: String,
    pub message: String,
    pub date: String,
}

/// Information about a worktree
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::TempDir;

    async fn run_git_checked(path: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("git")
            .current_dir(path)
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to start git {args:?}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    async fn init_test_repo() -> Result<(TempDir, GitManager)> {
        let temp = TempDir::new()?;

        // Initialize git repo
        run_git_checked(temp.path(), &["init"]).await?;
        run_git_checked(temp.path(), &["config", "user.email", "test@test.com"]).await?;
        run_git_checked(temp.path(), &["config", "user.name", "Test"]).await?;
        run_git_checked(temp.path(), &["config", "commit.gpgsign", "false"]).await?;

        // Add an initial commit so HEAD is valid
        run_git_checked(temp.path(), &["commit", "--allow-empty", "-m", "initial"]).await?;

        let manager = GitManager::new(temp.path());
        Ok((temp, manager))
    }

    #[tokio::test]
    async fn test_current_branch() {
        let (_temp, manager) = init_test_repo().await.unwrap();
        let branch = manager.current_branch().await.unwrap();
        // git init defaults to "master" or "main" depending on version
        assert!(
            branch == "master" || branch == "main",
            "expected master or main, got {branch}"
        );
    }

    #[tokio::test]
    async fn test_commit() {
        let (_temp, manager) = init_test_repo().await.unwrap();

        // Create a file
        std::fs::write(manager.repo_path.join("test.txt"), "hello").unwrap();

        // Commit
        let commit = manager.commit("Initial commit").await.unwrap();
        assert_eq!(commit.len(), 7); // Short hash
    }

    #[tokio::test]
    async fn test_is_dirty() {
        let (_temp, manager) = init_test_repo().await.unwrap();

        assert!(!manager.is_dirty().await.unwrap());

        std::fs::write(manager.repo_path.join("test.txt"), "hello").unwrap();

        assert!(manager.is_dirty().await.unwrap());
    }

    #[tokio::test]
    async fn isolated_worktree_is_detached_collision_safe_and_discard_is_non_destructive() {
        if std::env::var_os("NCA_AUTORESEARCH_GIT_HOOK_CHILD").is_none() {
            let output = StdCommand::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "git_integration::tests::isolated_worktree_is_detached_collision_safe_and_discard_is_non_destructive",
                    "--nocapture",
                ])
                .env("NCA_AUTORESEARCH_GIT_HOOK_CHILD", "1")
                .env("GIT_INDEX_FILE", ".git/index")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "worktree Git commands must ignore hook repository state: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let (_temp, manager) = init_test_repo().await.unwrap();
        let experiments = manager.repo_path.join("experiments");
        std::fs::write(manager.repo_path.join("user.txt"), "unrelated user work").unwrap();

        let workspace = manager
            .create_isolated_worktree(&experiments, "exp-1")
            .await
            .unwrap();

        assert_eq!(
            manager.worktree_head(&workspace).await.unwrap(),
            workspace.base_commit
        );
        assert!(
            manager
                .create_isolated_worktree(&experiments, "exp-1")
                .await
                .is_err()
        );

        std::fs::write(workspace.path.join("experiment.txt"), "candidate").unwrap();
        let changed = manager.workspace_changed_files(&workspace).await.unwrap();
        assert_eq!(changed, vec![PathBuf::from("experiment.txt")]);

        let disposition = manager
            .finalize_experiment(&workspace, WorktreeDecision::Discard)
            .await
            .unwrap();
        assert_eq!(disposition, WorktreeDisposition::Removed);
        assert!(!workspace.path.exists());
        assert_eq!(
            std::fs::read_to_string(manager.repo_path.join("user.txt")).unwrap(),
            "unrelated user work"
        );
    }

    #[tokio::test]
    async fn permitted_file_validation_rejects_unapproved_changes_and_keep_retains_workspace() {
        let (_temp, manager) = init_test_repo().await.unwrap();
        let workspace = manager
            .create_isolated_worktree(&manager.repo_path.join("experiments"), "exp-2")
            .await
            .unwrap();

        std::fs::write(workspace.path.join("allowed.txt"), "allowed").unwrap();
        assert_eq!(
            manager
                .validate_permitted_files(&workspace, &[PathBuf::from("allowed.txt")])
                .await
                .unwrap(),
            vec![PathBuf::from("allowed.txt")]
        );

        std::fs::write(workspace.path.join("secret.txt"), "not allowed").unwrap();
        let error = manager
            .validate_permitted_files(&workspace, &[PathBuf::from("allowed.txt")])
            .await
            .unwrap_err();
        assert!(error.to_string().contains("secret.txt"));

        assert_eq!(
            manager
                .finalize_experiment(&workspace, WorktreeDecision::Keep)
                .await
                .unwrap(),
            WorktreeDisposition::Retained
        );
        assert!(workspace.path.exists());

        manager
            .finalize_experiment(&workspace, WorktreeDecision::Failure)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn interrupted_worktree_can_be_recovered_by_identity_after_restart() {
        let (_temp, manager) = init_test_repo().await.unwrap();
        let experiments = manager.repo_path.join("experiments");
        let interrupted = manager
            .create_isolated_worktree(&experiments, "recover-me")
            .await
            .unwrap();
        std::fs::write(interrupted.path.join("candidate.txt"), "partial run").unwrap();

        // A fresh manager models a process restart. Recovery returns the
        // same workspace and preserves the interrupted candidate state.
        let restarted = GitManager::new(&manager.repo_path);
        let recovered = restarted
            .recover_isolated_worktree(&experiments, "recover-me")
            .await
            .unwrap();
        assert_eq!(recovered, interrupted);
        assert_eq!(
            std::fs::read_to_string(recovered.path.join("candidate.txt")).unwrap(),
            "partial run"
        );
        assert_eq!(
            restarted.workspace_changed_files(&recovered).await.unwrap(),
            vec![PathBuf::from("candidate.txt")]
        );

        // Normal creation still refuses the occupied id; callers must opt
        // into recovery and pass the identity checks above.
        assert!(
            restarted
                .create_isolated_worktree(&experiments, "recover-me")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn recovery_refuses_unregistered_or_tampered_experiment_state() {
        let (_temp, manager) = init_test_repo().await.unwrap();
        let experiments = manager.repo_path.join("experiments");
        std::fs::create_dir_all(experiments.join("not-a-worktree")).unwrap();
        assert!(
            manager
                .recover_isolated_worktree(&experiments, "not-a-worktree")
                .await
                .is_err()
        );

        let workspace = manager
            .create_isolated_worktree(&experiments, "tampered")
            .await
            .unwrap();
        let marker = workspace.path.join(EXPERIMENT_METADATA_FILE);
        let mut metadata: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&marker).unwrap()).unwrap();
        metadata["id"] = serde_json::Value::String("another-experiment".into());
        std::fs::write(&marker, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let error = manager
            .recover_isolated_worktree(&experiments, "tampered")
            .await
            .expect_err("a marker for another experiment must never be reused");
        assert!(error.to_string().contains("identity marker"));

        manager
            .finalize_experiment(&workspace, WorktreeDecision::Failure)
            .await
            .unwrap();
    }
}
