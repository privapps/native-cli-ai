# Autoresearch Git test stability

**Type:** Test-stability implementation plan
**Status:** Implemented locally
**Date:** 2026-07-31
**Related:** [Autoresearch implementation plan](autoresearch.md)
**Last verified:** 2026-07-31 (documentation review)

## Diagnosis

The autoresearch tests create temporary Git repositories and run commits without
checking their exit status. The test process inherits the developer's global
Git configuration, including `commit.gpgsign=true`; when signing is unavailable,
the setup commit fails but the tests continue until a later `worktree add`
reports an unrelated repository/index error.

## Plan

1. Make every temporary-repository setup command fail fast with its stderr
   included in the test failure.
2. Configure temporary repositories with local identity and
   `commit.gpgsign=false`, so tests are independent of user Git configuration.
3. Run the focused worktree and isolated-runner tests, then the full
   `nca-autoresearch` test suite with the normal environment and with repeated
   runs to verify the original failure is gone.
4. Make CLI spawn IPC publication tolerant of full-suite startup contention,
   then verify the pre-commit hook's complete validation sequence.
5. Make the spawn waiter fail fast with the child process' startup log when
   the child exits before publishing its endpoint.
6. Avoid Git's checkout path during worktree registration, then materialize
   the baseline explicitly inside the registered worktree.

## Verification

- All three reported tests pass under the normal environment.
- Five repeated full `nca-autoresearch` runs pass under the normal environment.
- Twenty repeated full `nca-autoresearch` runs pass after worktree creation was
  changed to register first and materialize the baseline explicitly.
- The full `nca-autoresearch` suite passes: 41 unit tests, 5 integration tests,
  1 lifecycle integration test, and doctests.
- `cargo fmt --all -- --check` and `git diff --check` pass.
- `cargo clippy -p nca-autoresearch -- -D warnings` passes.
- The pre-commit hook's test phase exposed a separate flaky CLI spawn timeout;
  its focused test passes in isolation, so the follow-up is to increase the
  bounded IPC publication grace period, fail fast with child startup logs, and
  re-run the hook.
- The complete pre-commit hook passes with normal filesystem permissions:
  workspace tests, clippy with warnings denied, and formatting.

## Scope

This hardens Git test fixtures and the isolated Git/CLI lifecycle. The user's
working tree, staging area, and global Git configuration are unchanged.
