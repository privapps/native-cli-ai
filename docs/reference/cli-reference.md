# CLI Reference

**Status:** Generated snapshot  
**Source:** `nca --help` from the Clap command definitions

This page is generated from the current CLI. For task-oriented workflows and examples, see the [user command guide](../user/commands.md).

```text
Native CLI AI - a Rust-powered general-purpose AI assistant

Usage: nca [OPTIONS] [COMMAND]

Commands:
  run           
  spawn         
  sessions      
  resume        
  logs          
  attach        
  status        
  cancel        
  skills        Manage skills: list, add, remove, update
  mcp           
  memory        
  models        
  doctor        
  config        
  completion    Generate shell completions for bash, zsh, fish, or PowerShell
  autoresearch  Autonomous research helpers (see `crates/autoresearch`, program `.md` files)
  index         Build or show a cached CLI index under the product home's workspaces/<id>/ directory (for agents and tooling)
  help          Print this message or the help of the given subcommand(s)

Options:
  -p, --prompt <PROMPT>
          One-shot prompt mode
  -s, --safe
          Start in read-only safe mode
      --yolo
          Disable nca-level approval and safety guards for this invocation
  -r, --resume
          Resume the last session
      --no-resume
          Start a new session instead of resuming the last one
      --run
          Start interactive run mode (Claude-style)
      --model <MODEL>
          Override the default model
  -t, --enable-thinking
          Enable extended thinking
      --thinking-budget <THINKING_BUDGET>
          Token budget for extended thinking [default: 5120]
      --reasoning-effort <REASONING_EFFORT>
          Reasoning effort for OpenAI-compatible Chat Completions or Responses requests
      --max-tokens <MAX_TOKENS>
          Max response tokens
  -v, --verbose
          Verbose debug logging
      --json
          Output structured JSON (for CI)
      --stream <STREAM>
          Streaming output format [default: human] [possible values: off, human, ndjson]
      --no-tui
          Line-oriented REPL instead of full-screen TUI (scripts, CI, or broken approval prompts)
      --permission-mode <PERMISSION_MODE>
          Permission handling mode (default: from config, fallback to `default`) [possible values: default, plan, accept-edits, dont-ask, bypass-permissions]
      --max-turns <MAX_TURNS>
          Max turns per run (overrides config)
  -h, --help
          Print help
```
