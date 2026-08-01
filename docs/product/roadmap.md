# nca Roadmap

This roadmap contains actionable follow-up work only. Completed capabilities are documented in the [user guide](../user/index.md) and [technical reference](../reference/index.md).

## Near term

- Verify lifecycle commands (`spawn`, `status`, `attach`, and `cancel`) under JSON and NDJSON automation.
- Normalize event schemas so machine-readable consumers can rely on stable envelopes.
- Improve IPC reconnect and error handling for long-running sessions.
- Add richer session search and filtering in the CLI.

## Later

- Add tmux and multiplexer awareness for long-running background workflows.
- Evaluate multi-directory context support for projects that span more than one workspace root.
- Decide whether future auxiliary clients need a transport beyond the current Unix-socket NDJSON protocol.
- Revisit session-file storage only if plain JSON becomes a measurable performance or compatibility constraint.

Implementation work should be captured in an active [design plan](../design/plans/) and linked back to the relevant product requirement or specification.
