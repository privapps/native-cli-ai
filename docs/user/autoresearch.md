# Autoresearch

Autoresearch is an explicit, bounded workflow for metric-driven experiments.
Program discovery is read-only: finding a Markdown program never runs its
commands. Execution requires a user-selected `/autoresearch` workflow (or an
explicit `nca autoresearch` command), and the normal tool approval policy still
applies.

## Program format

A program is Markdown with the following sections:

```markdown
# CLI latency research

Improve the measured command latency while preserving its behavior.

## Files
- Editable: `src/command.rs`
- Fixed: `Cargo.lock`

## Metric
- Command: `grep "latency_ms:" run.log`
- Regex: `latency_ms:\s*([0-9.]+)`
- Goal: minimize

## Constraints
- Time budget: 60 seconds
- Max memory: 4GB
- No network access

## Instructions
Change only the editable source and describe each experiment.
```

The required metric command identifies the output from which the metric is
parsed. When supplied, the regex must compile and contain a numeric capture;
when omitted, the parser uses the general numeric fallback `([\d.]+)`. `Goal`
is `minimize` or `maximize`. Constraint bullets other than time and memory are
retained as secondary declarations. Fixed files are never permitted in an
isolated experiment. During an isolated run, structured measurements and
`name: value`/`name=value` output are evaluated against those constraints
after the primary metric. A candidate that improves the primary metric but
violates a constraint is discarded; the audit record keeps both decisions and
the exact violation instead of labelling it as a primary regression. An
empty, malformed, or capture-less regex fails before an explicitly selected
session is persisted.

The agent supplies the direct executable and argument array for each bounded
continuation. This keeps the program declarative and prevents an unreviewed
shell string from becoming an implicit execution request.

## Agent contract

The `autoresearch` agent tool uses contract version `1` and these operations:

- `discover`: enumerate valid Markdown programs below the workspace; this
  returns `execution_started: false`.
- `start`: persist a session for a selected program without running an
  experiment.
- `status` and `results`: inspect durable state and rich failure context.
- `continue`: provide ordered experiment descriptions, a direct `command`,
  optional string `args`, and a policy object.
- `cancel`: stop a running session and preserve its evidence.

Each continuation is limited to at most 8 iterations and 3,600 seconds. Each
experiment is additionally limited by the program's per-experiment budget and
editable-file policy, and runs in a detached worktree. Interrupted runs can be
resumed explicitly using their durable session/experiment id; recovery refuses
missing, unregistered, tampered, or baseline-mismatched worktrees rather than
reusing an unknown directory. Results identify keep, discard, crash, timeout,
malformed-metric, and policy-failure evidence where available, including
captured stdout/stderr, changed files, primary decisions, and
secondary-constraint violations.

Example continuation input:

```json
{
  "operation": "continue",
  "session_id": "latency-1",
  "command": "cargo",
  "args": ["test", "--release", "latency_fixture"],
  "descriptions": ["avoid an unnecessary allocation"],
  "policy": {"max_iterations": 1, "max_total_seconds": 120}
}
```

The host marks the tool as execution-authorized only after explicit manual
skill selection. A model that merely sees the discovery tool, or that tries
to invoke `start`, `continue`, or `cancel` without that selection, receives a
policy refusal. `continue` is also a non-read-only tool, so default and
headless approval modes can require an interactive approval or fail clearly.

Sessions live below `.nca/autoresearch/sessions/` in the selected workspace.
Stopping or cancelling does not delete state, results, or audit records;
`results` can be used after a process restart to choose the next experiment.
