//! Bounded parallel scheduling for isolated autoresearch experiments.
//!
//! [`ParallelScheduler`] owns one workspace directory per job and limits the
//! number of in-flight runners.  Completion order is intentionally not the
//! result order: [`ParallelOutcome`] values are sorted by their input ordinal
//! before being returned, so a results file or terminal report remains
//! deterministic even when experiments finish at different times.

use std::future::Future;
use std::path::{Path, PathBuf};

use futures_util::stream::{self, StreamExt};

/// Errors found while preparing a parallel schedule.
#[derive(Debug, thiserror::Error)]
pub enum ParallelScheduleError {
    #[error("parallel experiment concurrency must be greater than zero")]
    InvalidConcurrency,
    #[error("failed to create parallel experiment workspace: {0}")]
    Io(#[from] std::io::Error),
}

/// A prepared experiment with a collision-safe private workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelExperiment {
    pub ordinal: usize,
    pub id: String,
    pub workspace: PathBuf,
}

/// One completed parallel result, retaining the experiment identity and
/// private workspace alongside the runner's value or error.
#[derive(Debug)]
pub struct ParallelOutcome<T, E> {
    pub experiment: ParallelExperiment,
    pub result: Result<T, E>,
}

/// A scheduler for a bounded batch of isolated experiment runners.
#[derive(Debug, Clone)]
pub struct ParallelScheduler {
    workspace_root: PathBuf,
    max_concurrency: usize,
}

impl ParallelScheduler {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        max_concurrency: usize,
    ) -> Result<Self, ParallelScheduleError> {
        if max_concurrency == 0 {
            return Err(ParallelScheduleError::InvalidConcurrency);
        }
        let workspace_root = workspace_root.into();
        std::fs::create_dir_all(&workspace_root)?;
        Ok(Self {
            workspace_root,
            max_concurrency,
        })
    }

    pub fn max_concurrency(&self) -> usize {
        self.max_concurrency
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Prepare one isolated workspace per logical experiment.
    pub fn prepare<I, S>(&self, ids: I) -> Result<Vec<ParallelExperiment>, ParallelScheduleError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        ids.into_iter()
            .enumerate()
            .map(|(ordinal, id)| {
                let id = id.into();
                let directory = format!("{:04}-{}", ordinal, safe_slug(&id));
                let workspace = self.workspace_root.join(directory);
                // Refuse to reuse a prior job directory.  A caller that
                // wants a fresh batch can choose a fresh root; silently
                // sharing state would invalidate experiment isolation.
                std::fs::create_dir(&workspace)?;
                Ok(ParallelExperiment {
                    ordinal,
                    id,
                    workspace,
                })
            })
            .collect()
    }

    /// Run jobs with bounded concurrency and return them in input order.
    pub async fn run<F, Fut, T, E>(
        &self,
        jobs: Vec<ParallelExperiment>,
        runner: F,
    ) -> Vec<ParallelOutcome<T, E>>
    where
        F: Fn(ParallelExperiment) -> Fut + Clone,
        Fut: Future<Output = Result<T, E>>,
    {
        let mut outcomes = stream::iter(jobs.into_iter().map(|job| {
            let runner = runner.clone();
            async move {
                let result = runner(job.clone()).await;
                ParallelOutcome {
                    experiment: job,
                    result,
                }
            }
        }))
        .buffer_unordered(self.max_concurrency)
        .collect::<Vec<_>>()
        .await;

        outcomes.sort_by_key(|outcome| outcome.experiment.ordinal);
        outcomes
    }
}

fn safe_slug(id: &str) -> String {
    let mut slug = String::with_capacity(id.len());
    for character in id.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
            slug.push(character);
        } else {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "experiment".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[tokio::test]
    async fn scheduling_is_bounded_isolated_and_deterministically_aggregated() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = ParallelScheduler::new(temp.path().join("experiments"), 2).unwrap();
        let jobs = scheduler
            .prepare(["first", "second/unsafe", "third"])
            .unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let outcomes = scheduler
            .run(jobs, {
                let active = active.clone();
                let peak = peak.clone();
                move |job| {
                    let active = active.clone();
                    let peak = peak.clone();
                    async move {
                        let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(current, Ordering::SeqCst);
                        std::fs::write(job.workspace.join("state"), &job.id).unwrap();
                        tokio::time::sleep(Duration::from_millis(
                            (3usize.saturating_sub(job.ordinal)) as u64 * 10,
                        ))
                        .await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok::<_, ()>(job.id)
                    }
                }
            })
            .await;

        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.experiment.ordinal)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(
            outcomes
                .iter()
                .map(|outcome| outcome.result.as_ref().unwrap().as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second/unsafe", "third"]
        );
        assert!(
            outcomes[1]
                .experiment
                .workspace
                .ends_with("0001-second-unsafe")
        );
        for outcome in outcomes {
            assert_eq!(
                std::fs::read_to_string(outcome.experiment.workspace.join("state")).unwrap(),
                outcome.experiment.id
            );
        }
    }

    #[test]
    fn zero_concurrency_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        assert!(matches!(
            ParallelScheduler::new(temp.path(), 0),
            Err(ParallelScheduleError::InvalidConcurrency)
        ));
    }

    #[test]
    fn an_existing_job_workspace_is_never_reused() {
        let temp = tempfile::tempdir().unwrap();
        let scheduler = ParallelScheduler::new(temp.path(), 1).unwrap();
        scheduler.prepare(["same-job"]).unwrap();
        assert!(scheduler.prepare(["same-job"]).is_err());
    }
}
