# Tools

nca's agent has access to a set of built-in tools for interacting with your codebase, running commands, and searching the web. Tools are the actions the agent can take on your behalf.

## Tool Categories

### Read-Only Tools

These tools are always available, including in [safe mode](./permissions.md).

| Tool | Description |
|------|-------------|
| `read_file` | Read the contents of a file |
| `search_code` | Search code using ripgrep with structured JSON output |
| `list_directory` | List files and directories at a path |
| `git_status` | Show `git status` for the workspace |
| `git_diff` | Show `git diff` (staged or unstaged) |
| `web_search` | Search the web via Bing with DuckDuckGo fallback, retaining URL, authority, retrieval, and available publication metadata |
| `fetch_url` | Fetch and extract text content from a URL, retaining source and publication metadata |

Financial-report tools are registered with the read-only tool set but remain
hidden until the agent explicitly invokes the `financial-research` skill. A
YOLO session enables the capability automatically. `write_validated_financial_report`
is a separate write tool and is available only with the full tool set and the
usual write permissions.

### Financial Research Tools

| Tool | Description |
|------|-------------|
| `resolve_latest_financial_report` | Resolve the newest eligible observed result for an issuer and cadence, with explicit fallback status and limitations |
| `validate_financial_report` | Validate a reported financial-period candidate against the current as-of boundary and observed official evidence |

### Write Tools

Available in standard mode. Requires appropriate [permissions](./permissions.md).

| Tool | Description |
|------|-------------|
| `write_file` | Create or overwrite a file |
| `create_directory` | Create a directory (including parents) |
| `apply_patch` | Apply one or more exact string replacements to a file |
| `edit_file` | Replace a specific string in an existing file |
| `replace_match` | Precision replace using line and column coordinates |
| `rename_path` | Rename a file or directory |
| `move_path` | Move a file or directory |
| `copy_path` | Copy a file |
| `delete_path` | Delete a file or directory |
| `write_validated_financial_report` | Write a report only when the current turn has a validated official report and complete verification metadata |

### Execution Tools

| Tool | Description |
|------|-------------|
| `execute_bash` | Execute a shell command in the workspace (PTY-backed) |
| `run_validation` | Run an allowlisted build/test/lint command |

### Intelligence Tools

| Tool | Description |
|------|-------------|
| `query_symbols` | Search for symbol definitions in the codebase |
| `ask_question` | Ask the user a structured question with options |
| `invoke_skill` | Load and follow a skill's instructions |
| `spawn_subagent` | Spawn a child agent session for parallel work |

### MCP Tools

Tools from configured [MCP servers](./advanced.md#mcp-servers) appear as `mcp__<server>__<tool>`.

---

## Tool Reference

### `read_file`

Read the contents of a file in the workspace.

**Parameters:**
- `path` (string, required) — File path relative to workspace root

**Behavior:** Reads the file asynchronously. The path must resolve within the workspace boundary.

---

### `search_code`

Search code using ripgrep and return structured match results.

**Parameters:**
- `pattern` (string, required) — Search pattern (regex by default)
- `path` (string, optional) — Directory to search in (default: workspace root)
- `glob` (string, optional) — File glob filter (e.g., `"*.rs"`)
- `fixed_strings` (bool, optional) — Treat pattern as literal text
- `case_sensitive` (bool, optional) — Case-sensitive matching
- `word` (bool, optional) — Match whole words only
- `context_before` (int, optional) — Lines of context before match
- `context_after` (int, optional) — Lines of context after match
- `max_results` (int, optional) — Maximum number of results

**Behavior:** Invokes `rg` with JSON output. Search root is validated to stay within the workspace.

---

### `list_directory`

List files and directories at a given path.

**Parameters:**
- `path` (string, optional) — Directory path (default: `.`, workspace root)

**Behavior:** Lists entries under the given path. Directories are suffixed with `/`.

---

### `git_status`

Show the current git status for the workspace.

**Parameters:** None (empty object `{}`)

**Behavior:** Runs `git status --short --branch` in the workspace.

---

### `git_diff`

Show git diff for the workspace.

**Parameters:**
- `staged` (bool, optional) — If true, show staged changes (`--cached`)

---

### `web_search`

Search the public web and return titles, URLs, snippets, and provenance metadata.

**Parameters:**
- `query` (string, required) — Search query
- `limit` (int, optional) — Number of results (1–10, default from config)
- `domains` (array of strings, optional) — Domains to add as search hints, such as an investor-relations site or `sec.gov`; domain hints do not change the neutral evidence contract
- `as_of` is supplied by the runtime turn context and is not a caller-controlled field

**Behavior:** Searches Bing RSS first, then falls back to DuckDuckGo HTML when Bing is empty, blocked, malformed, or unavailable. Bing uses a GET RSS request with `q`, English locale, and RSS-format query parameters. DuckDuckGo uses the Capelin-compatible POST form profile and resolves its redirect URLs to destination URLs. Transport failures and retryable HTTP statuses are retried inside the provider operation. DuckDuckGo additionally uses one process-wide serialized limiter shared by runtime sessions, with request-start spacing and bounded challenge cooldown; Bing does not use that limiter. Results are returned as neutral evidence in JSON with the query, date-only UTC turn `as_of`, retrieval timestamp, URL, source authority, available publication metadata, and an `eligible_as_of` flag. Unknown metadata remains `null`; the upstream search response is not assumed to support an exact date filter. The tool does not accept an issuer or infer financial report meaning; financial policy interprets the recorded evidence when a financial workflow uses it. If both providers fail, the tool returns one provider-aware failure and the agent does not repeat the exhausted search operation automatically.

#### Search failure classification and recovery

Search failures retain the reason from each provider so an agent can recover
without treating every failed response as malformed markup:

| Failure | Meaning | Handling |
|---------|---------|----------|
| `no usable results` | The provider returned a legitimate empty-results page | Falls back to the other provider; it is not reported as a parser failure |
| Parser failure (for example, `response contained no recognized structured results`) | The response was non-empty but did not match the provider parser | Falls back immediately; inspect the other provider or use `fetch_url` |
| `provider returned an anti-bot challenge (HTTP <status>)` | The provider blocked the request or presented a challenge | Recognized DuckDuckGo HTTP 202 challenges use the configured challenge retry budget and shared cooldown; other blocking is surfaced as non-retryable |
| Transport or retryable HTTP failure | The request could not complete or the provider returned 408, 425, 429, or 5xx | Retries according to the search retry budget and cooldown settings |

If both providers fail, `web_search` returns one error containing both provider
reasons and the agent stops that search operation after the failed call. The
agent reports recovery guidance to try a different provider or adjust the
query; it does not issue three equivalent `web_search` calls automatically.
See [Configuration](./configuration.md#web-web-request-settings) for retry,
cooldown, and request-spacing settings.

---

### `fetch_url`

Fetch and normalize the text content of a URL while preserving provenance.

**Parameters:**
- `url` (string, required) — The URL to fetch

**Behavior:** Makes an HTTP GET request, records the final URL, response status, retrieval timestamp, HTTP `Date` header when present, source authority, and common publication/report metadata, then strips HTML to text content and truncates to `max_fetch_chars` (default 25,000 characters). The result is JSON containing neutral evidence in `source` metadata and normalized `content`; `source.as_of` is the immutable date-only UTC boundary for the current turn, and `source.eligible_as_of` is `true` only when the observed publication date is on or before that date. Missing metadata remains `null`. The tool does not interpret issuer identity or financial-report eligibility; financial policy evaluates those properties from the recorded evidence.

---

### `validate_financial_report`

Validates a financial report candidate against the current per-turn date-only `as_of` and evidence recorded by `web_search` or `fetch_url`. The candidate must name its issuer, report type, reporting calendar, period end, status, publication URL, and publication date; observed evidence must also expose matching period metadata. Validation succeeds only when the period has ended, the source was observed as an official investor-relations or regulatory source, the publication date's UTC calendar date is no later than `as_of`, and the status is `reported` or `filed`. Guidance, estimates, future periods, secondary-only sources, undated sources, and sources with unknown period metadata fail explicitly.

Use the returned verified record when composing a financial report. Include the date-only `as_of`, fiscal/calendar interpretation, period end, report status, publication date, source retrieval timestamp, and official source URL in the final output.

### `resolve_latest_financial_report`

Resolves the newest eligible result already observed by `web_search` or `fetch_url`. The required `issuer` identifies the report being requested and `cadence` is `latest`, `annual`, or `quarterly`. Results are ranked by completed period end, then publication time, then source authority; a year token or search-result order is not used.

An `annual` request uses the newest eligible annual result when one exists. If a newer observed annual period is future, unpublished, or otherwise ineligible, an older annual is returned as a `fallback` with a limitation. If no eligible annual result is available but a verified quarter is available, the tool returns `status: "fallback"`, the quarterly record, and a limitation explaining that it must not be treated as annual. A `latest` request selects the newest eligible result regardless of cadence and still reports its actual type. An unavailable result is returned explicitly with `status: "unavailable"` and a limitation rather than fabricating a report. Conflicting official evidence is retained in `conflicts`, returns `status: "conflict"` with no selected report, and remains visible for reconciliation.

When a financial-looking final response cannot satisfy the verification metadata requirements, nca annotates structured JSON with `verification_status: "unverified"` and a `verification_warning`; it does not silently promote the response to verified. The fallback or unavailable limitation remains part of the structured resolution.

The same as-of, verification, provenance, and fallback semantics are carried
through human output, `--json`, and `--stream ndjson`. Human output renders the
content for the terminal, while JSON and NDJSON preserve the fields for
automation. A resumed session refreshes the turn's as-of date before new
evidence is evaluated; it does not reuse stale research context from the prior
turn.

---

### `write_file`

Create or overwrite a file inside the workspace.

**Parameters:**
- `path` (string, required) — File path relative to workspace
- `content` (string, required) — File contents

**Behavior:** Creates parent directories if needed. Path must resolve within workspace. Generic `write_file` does not inspect or classify content. Use `write_validated_financial_report` for financial output; it requires a validated report from the current research turn and refuses before changing the filesystem when verification is absent.

---

### `write_validated_financial_report`

Writes a report inside the workspace only after the current turn has produced
an eligible, official validated report. The content must disclose the shared
`as_of` date, issuer, reporting calendar, report type, period end, publication
status and date, source retrieval timestamp, and source URL. Missing evidence,
an ineligible period, a secondary-only source, incomplete metadata, or a
required cadence fallback that is not disclosed causes an explicit refusal
before the target file is touched.

---

### `create_directory`

Create a directory inside the workspace.

**Parameters:**
- `path` (string, required) — Directory path

**Behavior:** Creates the directory and all parent directories (`mkdir -p` equivalent).

---

### `apply_patch`

Apply one or more exact string replacements to a file.

**Parameters:**
- `path` (string, required) — File to patch
- `edits` (array, required) — List of edits, each containing:
  - `old_text` (string, required) — Text to find (must not be empty)
  - `new_text` (string, required) — Replacement text
  - `replace_all` (bool, optional) — Replace all occurrences (default: false)

**Behavior:** For each edit, finds the exact `old_text` string. If `replace_all` is false and multiple matches exist, the edit fails with an error.

---

### `edit_file`

Replace a specific string in an existing file.

**Parameters:**
- `path` (string, required) — File to edit
- `old_text` (string, required) — Text to find
- `new_text` (string, required) — Replacement text
- `replace_all` (bool, optional) — Replace all occurrences

Similar to `apply_patch` but for a single edit.

---

### `replace_match`

Precision replacement using exact file path, line number, and column.

**Parameters:**
- `path` (string, required) — File path
- `line` (int, required) — Line number (1-based)
- `column` (int, required) — Column number (1-based)
- `old_text` (string, required) — Text to replace at the specified position
- `new_text` (string, required) — Replacement text

**Behavior:** Anchors the replacement at a specific line and column for maximum precision.

---

### `rename_path`

Rename a file or directory within the workspace.

**Parameters:**
- `from` (string, required) — Current path
- `to` (string, required) — New path

---

### `move_path`

Move a file or directory within the workspace.

**Parameters:**
- `from` (string, required) — Source path
- `to` (string, required) — Destination path

---

### `copy_path`

Copy a file within the workspace.

**Parameters:**
- `from` (string, required) — Source file
- `to` (string, required) — Destination file

---

### `delete_path`

Delete a file or directory.

**Parameters:**
- `path` (string, required) — Path to delete
- `recursive` (bool, optional) — Required for directory deletion

**Behavior:** Always requires explicit approval under most permission modes (classified as destructive).

---

### `execute_bash`

Execute a shell command in the workspace.

**Parameters:**
- `command` (string, required) — The shell command to run
- `timeout_secs` (int, optional, default: 30) — Command timeout in seconds

**Behavior:** Runs in a PTY (pseudo-terminal) for full interactive command support. Returns stdout content or status message. Exit code 0 = success.

In safe mode, `execute_bash` is added to the deny list automatically.

---

### `run_validation`

Run a safe build, test, or lint command.

**Parameters:**
- `command` (string, required) — The command to run
- `cwd` (string, optional, default: `.`) — Working directory
- `timeout_secs` (int, optional, default: 120) — Command timeout

**Behavior:** Only executes commands that start with an allowlisted prefix:

- `cargo build`, `cargo test`, `cargo check`, `cargo clippy`, `cargo fmt`
- `npm run`, `npm test`, `npx`
- `pnpm run`, `pnpm test`
- `pytest`, `python -m pytest`
- `go test`, `go build`, `go vet`
- `make`

Other commands are rejected with an error.

---

### `query_symbols`

Search for symbol definitions (functions, structs, traits, etc.) in the codebase.

**Parameters:**
- `query` (string, required) — Symbol name or pattern to search
- `glob` (string, optional) — File filter

**Behavior:** Uses fast local code intelligence to find symbol definitions. Returns `path:line:text` formatted results.

---

### `ask_question`

Ask the user a structured question with predefined options.

**Parameters:**
- `prompt` (string, required) — The question text
- `options` (array, required) — List of options with `id` and `label`
- `suggested_answer` (string, required) — Default/recommended answer
- `allow_custom` (bool, optional, default: true) — Allow freeform custom answer

**Behavior:** Opens a modal in the TUI (or prompts in REPL) and waits for the user's selection. Blocks up to 3600 seconds.

---

### `invoke_skill`

Load a skill's full instructions by name.

`invoke_skill` is separate from the interactive `$skill` reference syntax.
Typing `$skill` supplies bounded, labeled guidance only; it does not execute
this tool or grant its permission. Use the existing `/skill` command or this
tool's normal authorization path when skill execution/loading is explicitly
requested.

**Parameters:**
- `skill_name` (string, required) — Name of the skill to invoke

**Behavior:** Discovers the skill's `SKILL.md` file from configured skill directories and returns its expanded content. If no match, lists available skills.

---

### `spawn_subagent`

Spawn a child agent session for parallel task delegation.

**Parameters:**
- `task` (string, required) — Clear description of what the sub-agent should do
- `focus_files` (string[], optional) — File paths the sub-agent should focus on
- `use_worktree` (bool, optional, default: true) — Run in an isolated git worktree

**Behavior:** Creates a new child session that inherits conversation context and the parent's authorization context, with no interactive approvals. A non-YOLO child fails loudly if its inherited policy requires approval; a YOLO child remains YOLO. Returns a JSON response with `child_session_id`, `status`, `output`, `workspace`, `branch`, and `worktree_path`. Times out after 600 seconds.

See [Sub-Agents](./advanced.md#sub-agents) for details.

---

## Tool Execution Flow

1. The LLM decides to call a tool and provides parameters
2. nca checks [permissions](./permissions.md) for the tool
3. If permission is `Ask`, the user is prompted for approval
4. The tool executes asynchronously
5. Results are fed back to the LLM for the next turn
6. Multiple approved tools can execute concurrently within a single turn

## Workspace Sandbox

All file tools enforce a workspace boundary:

- File reads, writes, and edits must resolve to paths within the workspace root
- Shell commands (`execute_bash`) run with the workspace as the working directory
- Attempts to access paths outside the workspace are rejected with an error

This sandbox protects against accidental or malicious file access outside your project.
