mod approval_prompts;
mod cli_index;
mod stream;

use crate::approval_prompts::InteractiveIpcApprovalHandler;
use clap::CommandFactory;
use clap::Parser;
use clap_complete::aot::generate;
use nca_common::config::{
    NcaConfig, PermissionMode, ProviderKind, resolve_memory_path, resolve_sessions_dir,
};
use nca_common::event::EndReason;
use nca_common::event::{AgentCommand, EventEnvelope};
use nca_common::execution::ExecutionContext;
use nca_common::session::{OrchestrationContext, SessionSnapshot, SessionStatus};
use nca_core::skills::SkillCatalog;
use nca_runtime::memory_store::{MemoryNote, MemoryStore};
use nca_tui::Repl;
use nca_tui::{build_resumed_session_runtime, build_session_runtime};
use std::io::{IsTerminal, stdin, stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use stream::{StreamMode, spawn_stream_task};

#[derive(Parser, Debug)]
#[command(
    name = "nca",
    about = "Native CLI AI - a Rust-powered general-purpose AI assistant"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// One-shot prompt mode
    #[arg(short, long)]
    prompt: Option<String>,

    /// Start in read-only safe mode
    #[arg(short, long)]
    safe: bool,

    /// Disable nca-level approval and safety guards for this invocation.
    #[arg(long, global = true)]
    yolo: bool,

    /// Resume the last session
    #[arg(short, long)]
    resume: bool,

    /// Start a new session instead of resuming the last one
    #[arg(long)]
    no_resume: bool,

    /// Start interactive run mode (Claude-style)
    #[arg(long)]
    run: bool,

    /// Override the default model
    #[arg(long)]
    model: Option<String>,

    /// Enable extended thinking
    #[arg(short = 't', long)]
    enable_thinking: bool,

    /// Token budget for extended thinking
    #[arg(long, default_value = "5120")]
    thinking_budget: u32,

    /// Reasoning effort for OpenAI-compatible Chat Completions or Responses requests
    #[arg(long, global = true)]
    reasoning_effort: Option<String>,

    /// Max response tokens
    #[arg(long)]
    max_tokens: Option<u32>,

    /// Verbose debug logging
    #[arg(short, long)]
    verbose: bool,

    /// Output structured JSON (for CI)
    #[arg(long)]
    json: bool,

    /// Streaming output format
    #[arg(long, value_enum, default_value_t = StreamMode::Human)]
    stream: StreamMode,

    /// Line-oriented REPL instead of full-screen TUI (scripts, CI, or broken approval prompts)
    #[arg(long)]
    no_tui: bool,

    /// Permission handling mode (default: from config, fallback to `default`)
    #[arg(long, value_enum)]
    permission_mode: Option<CliPermissionMode>,

    /// Max turns per run (overrides config)
    #[arg(long)]
    max_turns: Option<u32>,

    /// Internal session identifier for spawned runs
    #[arg(long, hide = true)]
    session_id: Option<String>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    Run {
        #[arg(long)]
        prompt: String,
        #[arg(long, value_enum, default_value_t = StreamMode::Human)]
        stream: StreamMode,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        safe: bool,
        #[arg(long, value_enum)]
        permission_mode: Option<CliPermissionMode>,
        #[arg(long, hide = true)]
        session_id: Option<String>,
    },
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long, value_enum, default_value_t = StreamMode::Ndjson)]
        stream: StreamMode,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        safe: bool,
        // Serve (IPC-driven) defaults to accept-edits for non-interactive service sessions
        #[arg(long, value_enum, default_value_t = CliPermissionMode::AcceptEdits)]
        permission_mode: CliPermissionMode,
        #[arg(long, hide = true)]
        session_id: Option<String>,
    },
    Spawn {
        #[arg(long)]
        prompt: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        safe: bool,
        #[arg(long)]
        json: bool,
        #[arg(long, value_enum)]
        permission_mode: Option<CliPermissionMode>,
    },
    Sessions {
        #[arg(long)]
        json: bool,
        /// Filter sessions by status (running, completed, cancelled, failed)
        #[arg(long, value_enum)]
        status: Option<SessionStatusFilter>,
        /// Filter sessions updated in the last N hours
        #[arg(long)]
        since_hours: Option<u32>,
        /// Search sessions by content/pattern
        #[arg(long)]
        search: Option<String>,
        /// Limit number of sessions shown
        #[arg(long, default_value = "20")]
        limit: usize,
    },
    Resume {
        session_id: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        safe: bool,
        #[arg(long, value_enum, default_value_t = StreamMode::Human)]
        stream: StreamMode,
        #[arg(long)]
        no_tui: bool,
        #[arg(long, value_enum)]
        permission_mode: Option<CliPermissionMode>,
    },
    Logs {
        session_id: String,
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        json: bool,
    },
    Attach {
        session_id: String,
        #[arg(long)]
        json: bool,
    },
    Status {
        session_id: String,
        #[arg(long)]
        json: bool,
    },
    Cancel {
        session_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Manage skills: list, add, remove, update
    Skills {
        /// Output as JSON (shorthand for `nca skills list --json`)
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        command: Option<SkillsCommand>,
    },
    Mcp {
        #[arg(long)]
        json: bool,
    },
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
        #[arg(long)]
        json: bool,
    },
    Models {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Config {
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completions for bash, zsh, fish, or PowerShell
    Completion {
        /// Shell to generate completions for
        #[arg(value_enum, default_value_t = ClapShell::Bash)]
        shell: ClapShell,
    },
    /// Autonomous research helpers (see `crates/autoresearch`, program `.md` files).
    Autoresearch {
        #[command(subcommand)]
        command: AutoresearchCmd,
    },
    /// Build or show a cached CLI index under the product home's workspaces/<id>/ directory (for agents and tooling).
    Index {
        #[command(subcommand)]
        command: IndexCmd,
    },
}

#[derive(clap::Subcommand, Debug)]
enum IndexCmd {
    /// Generate `cli-index.json` for the current workspace (canonical path → stable id).
    Build {
        /// Print JSON status (path, workspace_id) instead of a one-line message
        #[arg(long)]
        json: bool,
    },
    /// Print the last generated index (requires `index build` first).
    Show {
        /// Pretty-print full JSON; otherwise print a short summary
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Subcommand, Debug)]
enum AutoresearchCmd {
    /// Start and persist a new autoresearch session.
    Start {
        /// Path to the research program markdown file.
        #[arg(long)]
        program: PathBuf,
        /// Workspace for the session (defaults to the current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Stable session identifier; one is generated when omitted.
        #[arg(long, alias = "id")]
        session_id: Option<String>,
        /// Output the durable session state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Inspect a persisted autoresearch session.
    Status {
        /// Session id; the newest session is used when omitted.
        session_id: Option<String>,
        /// Workspace for the session (defaults to the current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Output the durable session state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Stop a running autoresearch session without deleting its evidence.
    Stop {
        /// Session id; the newest session is used when omitted.
        session_id: Option<String>,
        /// Workspace for the session (defaults to the current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Output the durable session state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume a stopped autoresearch session.
    Resume {
        /// Session id; the newest session is used when omitted.
        session_id: Option<String>,
        /// Workspace for the session (defaults to the current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Output the durable session state as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List persisted results without rerunning the research program.
    Results {
        /// Session id; the newest session is used when omitted.
        session_id: Option<String>,
        /// Workspace for the session (defaults to the current directory).
        #[arg(long)]
        workspace: Option<PathBuf>,
        /// Output the session and results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Run the program's metric shell command once and print the parsed metric.
    Once {
        /// Path to research program markdown (e.g. `docs/research/cli-dx-research.md`)
        program: PathBuf,
        /// Working directory (defaults to current directory)
        #[arg(long)]
        workspace: Option<PathBuf>,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum ClapShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

#[derive(clap::Subcommand, Debug)]
enum MemoryCommand {
    List,
    Add {
        text: String,
        #[arg(long, default_value = "note")]
        kind: String,
    },
}

#[derive(clap::Subcommand, Debug)]
enum SkillsCommand {
    /// List installed skills
    List {
        #[arg(long)]
        json: bool,
    },
    /// Install skills from a GitHub repo or local path
    Add {
        /// Source: owner/repo, GitHub URL, or local path
        source: String,
        /// Install specific skills by name (default: all)
        #[arg(short, long)]
        skill: Vec<String>,
        /// Install to ~/.nca/skills/ instead of .nca/skills/
        #[arg(short, long)]
        global: bool,
    },
    /// Remove an installed skill
    Remove {
        /// Skill command name to remove
        name: String,
        /// Remove from ~/.nca/skills/ instead of .nca/skills/
        #[arg(short, long)]
        global: bool,
    },
    /// Update installed skills from their source
    Update {
        /// Specific skill to update (default: all)
        name: Option<String>,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum CliPermissionMode {
    Default,
    Plan,
    AcceptEdits,
    DontAsk,
    BypassPermissions,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
enum SessionStatusFilter {
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl From<CliPermissionMode> for PermissionMode {
    fn from(value: CliPermissionMode) -> Self {
        match value {
            CliPermissionMode::Default => Self::Default,
            CliPermissionMode::Plan => Self::Plan,
            CliPermissionMode::AcceptEdits => Self::AcceptEdits,
            CliPermissionMode::DontAsk => Self::DontAsk,
            CliPermissionMode::BypassPermissions => Self::BypassPermissions,
        }
    }
}

impl CliPermissionMode {
    fn as_arg(self) -> &'static str {
        match self {
            CliPermissionMode::Default => "default",
            CliPermissionMode::Plan => "plan",
            CliPermissionMode::AcceptEdits => "accept-edits",
            CliPermissionMode::DontAsk => "dont-ask",
            CliPermissionMode::BypassPermissions => "bypass-permissions",
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match try_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            classify_exit_code(&error)
        }
    }
}

async fn try_main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let filter = if cli.verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("nca starting");
    let mut config = NcaConfig::load()?;
    let orchestration_context = OrchestrationContext::from_env();
    let reasoning_effort_override = cli.reasoning_effort.clone();

    if let Some(model) = &cli.model {
        config.apply_model_override(model);
    }

    if let Some(max_tokens) = cli.max_tokens {
        config.model.max_tokens = max_tokens;
    }
    if cli.enable_thinking {
        config.model.enable_thinking = true;
        config.model.thinking_budget = cli.thinking_budget;
    }
    if let Some(reasoning_effort) = &reasoning_effort_override {
        config.model.reasoning_effort = reasoning_effort.clone();
    }
    if let Some(max_turns) = cli.max_turns {
        config.session.max_turns_per_run = max_turns;
    }

    let workspace_root = PathBuf::from(".");
    match cli.command {
        Some(Command::Run {
            prompt,
            stream,
            model,
            json,
            safe,
            permission_mode,
            session_id,
        }) => {
            reject_safe_yolo(safe, cli.yolo)?;
            if let Some(model) = model {
                config.apply_model_override(&model);
            }
            if let Some(mode) = permission_mode {
                config.permissions.mode = mode.into();
            }
            run_one_shot(
                config,
                &workspace_root,
                &prompt,
                OneShotOptions {
                    stream,
                    json,
                    safe,
                    yolo: cli.yolo,
                    session_id,
                    orchestration_context: orchestration_context.clone(),
                },
            )
            .await?;
        }
        Some(Command::Serve {
            prompt,
            stream,
            model,
            safe,
            permission_mode,
            session_id,
        }) => {
            reject_safe_yolo(safe, cli.yolo)?;
            if let Some(model) = model {
                config.apply_model_override(&model);
            }
            // Serve always uses an explicit default (accept-edits) — not overridable from config
            config.permissions.mode = permission_mode.into();
            run_service_session(
                config,
                &workspace_root,
                prompt,
                stream,
                safe,
                cli.yolo,
                session_id,
                orchestration_context.clone(),
            )
            .await?;
        }
        Some(Command::Spawn {
            prompt,
            model,
            safe,
            json,
            permission_mode,
        }) => {
            reject_safe_yolo(safe, cli.yolo)?;
            let effective_mode = permission_mode.unwrap_or(CliPermissionMode::AcceptEdits);
            config.permissions.mode = effective_mode.into();
            spawn_run(
                &workspace_root,
                &prompt,
                model
                    .as_deref()
                    .map(|model| config.model.resolve_alias(model)),
                reasoning_effort_override.as_deref(),
                safe,
                effective_mode,
                json,
                cli.yolo,
            )
            .await?;
        }
        Some(Command::Sessions {
            json,
            status,
            since_hours,
            search,
            limit,
        }) => {
            list_sessions(
                &config,
                &workspace_root,
                json,
                status,
                since_hours,
                search,
                limit,
            )
            .await?;
        }
        Some(Command::Resume {
            session_id,
            prompt,
            model,
            safe,
            stream,
            no_tui,
            permission_mode,
        }) => {
            if let Some(model) = model {
                config.apply_model_override(&model);
            }
            if let Some(mode) = permission_mode {
                config.permissions.mode = mode.into();
            }
            resume_session(
                config,
                &workspace_root,
                &session_id,
                prompt,
                safe,
                stream,
                no_tui,
                cli.yolo,
            )
            .await?;
        }
        Some(Command::Logs {
            session_id,
            follow,
            json,
        }) => {
            show_logs(&config, &workspace_root, &session_id, follow, json).await?;
        }
        Some(Command::Attach { session_id, json }) => {
            attach_session(&config, &workspace_root, &session_id, json).await?;
        }
        Some(Command::Status { session_id, json }) => {
            show_status(&config, &workspace_root, &session_id, json).await?;
        }
        Some(Command::Cancel { session_id, json }) => {
            cancel_session(&config, &workspace_root, &session_id, json).await?;
        }
        Some(Command::Skills { json, command }) => match command {
            None => {
                list_skills(&config, &workspace_root, json)?;
            }
            Some(SkillsCommand::List { json: j }) => {
                list_skills(&config, &workspace_root, j || json)?;
            }
            Some(SkillsCommand::Add {
                source,
                skill,
                global,
            }) => {
                handle_skills_add(&source, &skill, global, &workspace_root)?;
            }
            Some(SkillsCommand::Remove { name, global }) => {
                handle_skills_remove(&name, global, &workspace_root)?;
            }
            Some(SkillsCommand::Update { name }) => {
                handle_skills_update(name.as_deref(), &workspace_root)?;
            }
        },
        Some(Command::Mcp { json }) => {
            list_mcp_servers(&config, json)?;
        }
        Some(Command::Memory { command, json }) => match command {
            MemoryCommand::List => show_memory(&config, &workspace_root, json).await?,
            MemoryCommand::Add { text, kind } => {
                add_memory_note(&config, &workspace_root, &kind, &text, json).await?
            }
        },
        Some(Command::Models { json }) => {
            show_models(&config, json)?;
        }
        Some(Command::Doctor { json }) => {
            show_doctor(&config, &workspace_root, json)?;
        }
        Some(Command::Config { json }) => {
            show_config(&config, &workspace_root, json)?;
        }
        Some(Command::Completion { shell }) => {
            generate_shell_completion(shell);
        }
        Some(Command::Index { command }) => match command {
            IndexCmd::Build { json } => {
                cli_index::run_index_build(&workspace_root, json).await?;
            }
            IndexCmd::Show { json } => {
                cli_index::run_index_show(&workspace_root, json).await?;
            }
        },
        Some(Command::Autoresearch { command }) => match command {
            AutoresearchCmd::Start {
                program,
                workspace,
                session_id,
                json,
            } => {
                let workspace = resolve_autoresearch_workspace(workspace, &workspace_root);
                autoresearch_start(program, workspace, session_id, json)?;
            }
            AutoresearchCmd::Status {
                session_id,
                workspace,
                json,
            } => {
                let workspace = resolve_autoresearch_workspace(workspace, &workspace_root);
                autoresearch_status(workspace, session_id, json)?;
            }
            AutoresearchCmd::Stop {
                session_id,
                workspace,
                json,
            } => {
                let workspace = resolve_autoresearch_workspace(workspace, &workspace_root);
                autoresearch_stop(workspace, session_id, json)?;
            }
            AutoresearchCmd::Resume {
                session_id,
                workspace,
                json,
            } => {
                let workspace = resolve_autoresearch_workspace(workspace, &workspace_root);
                autoresearch_resume(workspace, session_id, json)?;
            }
            AutoresearchCmd::Results {
                session_id,
                workspace,
                json,
            } => {
                let workspace = resolve_autoresearch_workspace(workspace, &workspace_root);
                autoresearch_results(workspace, session_id, json)?;
            }
            AutoresearchCmd::Once { program, workspace } => {
                let ws = workspace.unwrap_or_else(|| workspace_root.clone());
                autoresearch_once(program, ws).await?;
            }
        },
        None => {
            reject_safe_yolo(cli.safe, cli.yolo)?;
            if let Some(prompt) = cli.prompt.as_deref() {
                if let Some(mode) = cli.permission_mode {
                    config.permissions.mode = mode.into();
                }
                if cli.run {
                    let ipc_approval = InteractiveIpcApprovalHandler::new();
                    let mut runtime = build_session_runtime(
                        config.clone(),
                        &workspace_root,
                        cli.safe,
                        cli.yolo,
                        true,
                        cli.session_id,
                        Some(ipc_approval.clone()),
                        orchestration_context.clone(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?;
                    if let Some(rx) = runtime.take_event_rx() {
                        let ipc_handle = runtime.take_ipc_handle();
                        let approval_pending = runtime.take_ipc_approval_pending();
                        let _stream_task = spawn_stream_task(
                            rx,
                            cli.stream,
                            runtime.event_log_path(),
                            ipc_handle,
                            approval_pending,
                            runtime.question_pending(),
                            None,
                        );
                        let _ = runtime.run_turn(prompt).await;
                        let mut repl = Repl::new(runtime, cli.safe, true);
                        repl.run().await?;
                    }
                } else {
                    run_one_shot(
                        config,
                        &workspace_root,
                        prompt,
                        OneShotOptions {
                            stream: cli.stream,
                            json: cli.json,
                            safe: cli.safe,
                            yolo: cli.yolo,
                            session_id: cli.session_id,
                            orchestration_context: orchestration_context.clone(),
                        },
                    )
                    .await?;
                }
            } else {
                // First-run onboarding: show connect modal before building runtime
                let onboarding_tui = !cli.no_tui
                    && stdout().is_terminal()
                    && stdin().is_terminal()
                    && matches!(cli.stream, StreamMode::Human);
                if config.needs_onboarding() && onboarding_tui {
                    config = nca_tui::tui::onboarding::run_onboarding(config).await?;
                }

                if cli.resume {
                    reject_safe_yolo(cli.safe, cli.yolo)?;
                    if let Some(mode) = cli.permission_mode {
                        config.permissions.mode = mode.into();
                    }
                    let session_id = latest_session_id(&config, &workspace_root).await?;
                    resume_session(
                        config,
                        &workspace_root,
                        &session_id,
                        None,
                        cli.safe,
                        cli.stream,
                        cli.no_tui,
                        cli.yolo,
                    )
                    .await?;
                } else if cli.no_resume {
                    reject_safe_yolo(cli.safe, cli.yolo)?;
                    // Explicitly skip auto-resume; create a fresh session.
                    if cli.run {
                        eprintln!("[run-mode] interactive run profile enabled");
                    }
                    if let Some(mode) = cli.permission_mode {
                        config.permissions.mode = mode.into();
                    }
                    let use_tui = !cli.no_tui
                        && stdout().is_terminal()
                        && stdin().is_terminal()
                        && matches!(cli.stream, StreamMode::Human);
                    let approval_handler: Option<
                        std::sync::Arc<dyn nca_core::approval::ApprovalHandler>,
                    > = if use_tui {
                        None
                    } else {
                        Some(InteractiveIpcApprovalHandler::new())
                    };
                    let mut runtime = build_session_runtime(
                        config.clone(),
                        &workspace_root,
                        cli.safe,
                        cli.yolo,
                        true,
                        cli.session_id,
                        approval_handler,
                        orchestration_context.clone(),
                    )
                    .await
                    .map_err(anyhow::Error::msg)?;
                    if !use_tui && let Some(rx) = runtime.take_event_rx() {
                        let ipc_handle = runtime.take_ipc_handle();
                        let approval_pending = runtime.take_ipc_approval_pending();
                        let _stream_task = spawn_stream_task(
                            rx,
                            cli.stream,
                            runtime.event_log_path(),
                            ipc_handle,
                            approval_pending,
                            runtime.question_pending(),
                            None,
                        );
                    }
                    let mut repl = Repl::new(runtime, cli.safe, cli.run);
                    if use_tui {
                        repl.run_with_tui().await?;
                    } else {
                        repl.run().await?;
                    }
                } else {
                    // Auto-resume: if a last-session pointer exists and points to a valid session,
                    // resume it silently instead of creating a new session.
                    if let Ok(Some(session_id)) =
                        nca_runtime::supervisor::get_last_session_id(&config, &workspace_root).await
                    {
                        reject_safe_yolo(cli.safe, cli.yolo)?;
                        if let Some(mode) = cli.permission_mode {
                            config.permissions.mode = mode.into();
                        }
                        eprintln!(
                            "[session] Resuming last session {} (use --no-resume to start fresh)",
                            session_id
                        );
                        resume_session(
                            config,
                            &workspace_root,
                            &session_id,
                            None,
                            cli.safe,
                            cli.stream,
                            cli.no_tui,
                            cli.yolo,
                        )
                        .await?;
                    } else {
                        if cli.run {
                            eprintln!("[run-mode] interactive run profile enabled");
                        }
                        if let Some(mode) = cli.permission_mode {
                            config.permissions.mode = mode.into();
                        }
                        let use_tui = !cli.no_tui
                            && stdout().is_terminal()
                            && stdin().is_terminal()
                            && matches!(cli.stream, StreamMode::Human);
                        let approval_handler: Option<
                            std::sync::Arc<dyn nca_core::approval::ApprovalHandler>,
                        > = if use_tui {
                            None
                        } else {
                            Some(InteractiveIpcApprovalHandler::new())
                        };
                        let mut runtime = build_session_runtime(
                            config.clone(),
                            &workspace_root,
                            cli.safe,
                            cli.yolo,
                            true,
                            cli.session_id,
                            approval_handler,
                            orchestration_context.clone(),
                        )
                        .await
                        .map_err(anyhow::Error::msg)?;
                        if !use_tui && let Some(rx) = runtime.take_event_rx() {
                            let ipc_handle = runtime.take_ipc_handle();
                            let approval_pending = runtime.take_ipc_approval_pending();
                            let _stream_task = spawn_stream_task(
                                rx,
                                cli.stream,
                                runtime.event_log_path(),
                                ipc_handle,
                                approval_pending,
                                runtime.question_pending(),
                                None,
                            );
                        }
                        let mut repl = Repl::new(runtime, cli.safe, cli.run);
                        if use_tui {
                            repl.run_with_tui().await?;
                        } else {
                            repl.run().await?;
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

struct OneShotOptions {
    stream: StreamMode,
    json: bool,
    safe: bool,
    yolo: bool,
    session_id: Option<String>,
    orchestration_context: Option<OrchestrationContext>,
}

async fn run_one_shot(
    config: NcaConfig,
    workspace_root: &Path,
    prompt: &str,
    opts: OneShotOptions,
) -> anyhow::Result<()> {
    let OneShotOptions {
        stream,
        json,
        safe,
        yolo,
        session_id,
        orchestration_context,
    } = opts;
    let mut runtime = build_session_runtime(
        config.clone(),
        workspace_root,
        safe,
        yolo,
        false,
        session_id,
        None,
        orchestration_context,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    if let Some(rx) = runtime.take_event_rx() {
        let ipc_handle = runtime.take_ipc_handle();
        let approval_pending = runtime.take_ipc_approval_pending();
        let stream_task = spawn_stream_task(
            rx,
            stream,
            runtime.event_log_path(),
            ipc_handle,
            approval_pending,
            runtime.question_pending(),
            None,
        );

        let spawn_task = runtime.take_spawn_rx().map(|spawn_rx| {
            nca_runtime::supervisor::spawn_subagent_consumer(
                spawn_rx,
                runtime.session_id().to_string(),
                runtime.workspace_root().to_path_buf(),
                config.clone(),
                ExecutionContext { yolo },
                safe,
                runtime.messages().to_vec(),
                None,
            )
        });

        let result = runtime.run_turn(prompt).await;
        let outcome = match result {
            Ok(output) => {
                runtime.finish(EndReason::Completed).await;
                if matches!(stream, StreamMode::Off) {
                    if json {
                        print_json(
                            &RunCommandOutput {
                                session: runtime.snapshot(),
                                output,
                                end_reason: "completed",
                            },
                            false,
                        )?;
                    } else {
                        println!("{output}");
                    }
                } else {
                    println!();
                    eprintln!("[session] {}", runtime.session_id());
                }
                Ok(())
            }
            Err(error) => {
                runtime.finish(EndReason::Error).await;
                Err(anyhow::Error::msg(error.to_string()))
            }
        };
        stream_task.abort();
        if let Some(st) = spawn_task {
            st.abort();
        }
        outcome?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_service_session(
    config: NcaConfig,
    workspace_root: &Path,
    initial_prompt: Option<String>,
    stream: StreamMode,
    safe: bool,
    yolo: bool,
    session_id: Option<String>,
    orchestration_context: Option<OrchestrationContext>,
) -> anyhow::Result<()> {
    let _ = stream;
    nca_runtime::service::run_service_session(nca_runtime::service::ServiceSessionRequest {
        config,
        workspace_root: workspace_root.to_path_buf(),
        safe_mode: safe,
        execution: ExecutionContext { yolo },
        initial_prompt,
        orchestration_context,
        kind: nca_runtime::service::ServiceSessionKind::New { session_id },
    })
    .await
    .map_err(anyhow::Error::msg)
}

#[allow(clippy::too_many_arguments)]
async fn spawn_run(
    workspace_root: &Path,
    prompt: &str,
    model: Option<String>,
    reasoning_effort: Option<&str>,
    safe: bool,
    permission_mode: CliPermissionMode,
    json: bool,
    yolo: bool,
) -> anyhow::Result<()> {
    let session_id = format!("session-{}", chrono::Utc::now().timestamp_millis());
    let config = NcaConfig::load_for_workspace(workspace_root).unwrap_or_default();
    let sessions_dir = resolve_sessions_dir(&config, workspace_root);
    std::fs::create_dir_all(&sessions_dir)?;
    let spawn_log = sessions_dir.join(format!("{session_id}.spawn.log"));
    let stdout = std::fs::File::create(&spawn_log)?;
    let stderr = stdout.try_clone()?;
    let exe = std::env::current_exe()?;

    let mut command = std::process::Command::new(exe);
    command.args(spawn_command_args(
        prompt,
        &session_id,
        reasoning_effort,
        model.as_deref(),
        safe,
        permission_mode,
        yolo,
    ));

    let mut child = command.stdout(stdout).stderr(stderr).spawn()?;
    let socket_path = if json {
        Some(
            wait_for_spawned_socket_path(&sessions_dir, &session_id, &mut child, &spawn_log)
                .await?,
        )
    } else {
        None
    };

    if json {
        print_json(
            &SpawnCommandOutput {
                session_id: session_id.clone(),
                pid: child.id(),
                status_path: sessions_dir.join(format!("{session_id}.json")),
                event_log_path: sessions_dir.join(format!("{session_id}.events.jsonl")),
                spawn_log_path: spawn_log,
                socket_path: socket_path.expect("JSON spawn should publish an IPC endpoint"),
                permission_mode: permission_mode.as_arg().to_string(),
                safe_mode: safe,
                yolo,
            },
            false,
        )?;
    } else {
        println!("{session_id}");
    }
    Ok(())
}

fn spawn_command_args(
    prompt: &str,
    session_id: &str,
    reasoning_effort: Option<&str>,
    model: Option<&str>,
    safe: bool,
    permission_mode: CliPermissionMode,
    yolo: bool,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(reasoning_effort) = reasoning_effort {
        args.push("--reasoning-effort".into());
        args.push(reasoning_effort.into());
    }
    args.extend([
        "run".into(),
        "--prompt".into(),
        prompt.into(),
        "--stream".into(),
        "ndjson".into(),
        "--session-id".into(),
        session_id.into(),
        "--permission-mode".into(),
        permission_mode.as_arg().into(),
    ]);
    if safe {
        args.push("--safe".into());
    }
    if yolo {
        args.push("--yolo".into());
    }
    if let Some(model) = model {
        args.push("--model".into());
        args.push(model.into());
    }
    args
}

async fn wait_for_spawned_socket_path(
    sessions_dir: &Path,
    session_id: &str,
    child: &mut std::process::Child,
    spawn_log: &Path,
) -> anyhow::Result<PathBuf> {
    // Spawning a child while the full test suite or a busy workstation is
    // under load can delay runtime initialization before the IPC endpoint is
    // persisted. Keep the wait bounded, but leave enough startup headroom for
    // the CLI's machine-readable spawn contract.
    const PUBLISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);

    let store = nca_runtime::session_store::SessionStore::new(sessions_dir);
    let deadline = tokio::time::Instant::now() + PUBLISH_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            let details = std::fs::read_to_string(spawn_log)
                .unwrap_or_default()
                .trim()
                .to_string();
            anyhow::bail!(
                "spawned session {session_id} exited with {status} before publishing its IPC endpoint{}",
                if details.is_empty() {
                    String::new()
                } else {
                    format!(": {details}")
                }
            );
        }

        if let Ok(session) = store.load(session_id).await
            && session.meta.pid == Some(child.id())
            && let Some(socket_path) = session.meta.socket_path
        {
            return Ok(socket_path);
        }

        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!(
                "spawned session {session_id} did not publish its IPC endpoint within {} seconds",
                PUBLISH_TIMEOUT.as_secs()
            );
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn list_sessions(
    config: &NcaConfig,
    workspace_root: &Path,
    json: bool,
    status_filter: Option<SessionStatusFilter>,
    since_hours: Option<u32>,
    search: Option<String>,
    limit: usize,
) -> anyhow::Result<()> {
    use nca_common::session::SessionStatus;

    let store =
        nca_runtime::session_store::SessionStore::new(resolve_sessions_dir(config, workspace_root));
    let ids = store.list().await.map_err(anyhow::Error::msg)?;
    let mut sessions = Vec::new();
    let mut unreadable = Vec::new();

    for id in ids {
        match store.load_snapshot(&id).await {
            Ok(session) => sessions.push(session),
            Err(_) => unreadable.push(id),
        }
    }

    // Apply status filter
    if let Some(status) = status_filter {
        sessions.retain(|s| match status {
            SessionStatusFilter::Running => matches!(s.status, SessionStatus::Running),
            SessionStatusFilter::Completed => matches!(s.status, SessionStatus::Completed),
            SessionStatusFilter::Cancelled => matches!(s.status, SessionStatus::Cancelled),
            SessionStatusFilter::Failed => matches!(s.status, SessionStatus::Error),
        });
    }

    // Apply time filter
    if let Some(hours) = since_hours {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        sessions.retain(|s| s.updated_at > cutoff);
    }

    // Apply search filter
    if let Some(pattern) = search {
        let pattern_lower = pattern.to_lowercase();
        sessions.retain(|s| {
            s.id.to_lowercase().contains(&pattern_lower)
                || s.session_summary
                    .as_ref()
                    .map(|sum| sum.to_lowercase().contains(&pattern_lower))
                    .unwrap_or(false)
                || s.model.to_lowercase().contains(&pattern_lower)
        });
    }

    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    // Apply limit
    sessions.truncate(limit);
    unreadable.sort();

    if json {
        print_json(
            &SessionListOutput {
                sessions,
                unreadable,
            },
            false,
        )?;
    } else {
        for session in sessions {
            print_human_session(&session);
        }
        for id in unreadable {
            println!("{id}\tUnreadable");
        }
    }
    Ok(())
}

async fn latest_session_id(config: &NcaConfig, workspace_root: &Path) -> anyhow::Result<String> {
    let store =
        nca_runtime::session_store::SessionStore::new(resolve_sessions_dir(config, workspace_root));
    let ids = store.list().await.map_err(anyhow::Error::msg)?;
    let mut latest = None;

    for id in ids {
        let Ok(session) = store.load(&id).await else {
            continue;
        };

        let should_replace = latest
            .as_ref()
            .map(|(_, updated_at)| session.meta.updated_at > *updated_at)
            .unwrap_or(true);
        if should_replace {
            latest = Some((session.meta.id, session.meta.updated_at));
        }
    }

    latest
        .map(|(id, _)| id)
        .ok_or_else(|| anyhow::anyhow!("no saved sessions found to resume"))
}

#[allow(clippy::too_many_arguments)]
async fn resume_session(
    config: NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    prompt: Option<String>,
    safe: bool,
    stream: StreamMode,
    no_tui: bool,
    yolo: bool,
) -> anyhow::Result<()> {
    reject_safe_yolo(safe, yolo)?;
    let use_tui = !no_tui
        && stdout().is_terminal()
        && stdin().is_terminal()
        && matches!(stream, StreamMode::Human);
    let approval_handler: Option<std::sync::Arc<dyn nca_core::approval::ApprovalHandler>> =
        if use_tui {
            None
        } else {
            Some(InteractiveIpcApprovalHandler::new())
        };
    let mut runtime = build_resumed_session_runtime(
        config,
        workspace_root,
        safe,
        yolo,
        true,
        session_id,
        approval_handler,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    if let Some(prompt) = prompt {
        if let Some(rx) = runtime.take_event_rx() {
            let ipc_handle = runtime.take_ipc_handle();
            let approval_pending = runtime.take_ipc_approval_pending();
            let _stream_task = spawn_stream_task(
                rx,
                stream,
                runtime.event_log_path(),
                ipc_handle,
                approval_pending,
                runtime.question_pending(),
                None,
            );
        }
        let output = runtime
            .run_turn(&prompt)
            .await
            .map_err(anyhow::Error::msg)?;
        println!("{output}");
        return Ok(());
    }

    if !use_tui && let Some(rx) = runtime.take_event_rx() {
        let ipc_handle = runtime.take_ipc_handle();
        let approval_pending = runtime.take_ipc_approval_pending();
        let _stream_task = spawn_stream_task(
            rx,
            stream,
            runtime.event_log_path(),
            ipc_handle,
            approval_pending,
            runtime.question_pending(),
            None,
        );
    }
    let mut repl = Repl::new(runtime, safe, true);
    if use_tui {
        repl.run_with_tui().await?;
    } else {
        repl.run().await?;
    }
    Ok(())
}

async fn show_logs(
    config: &NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    follow: bool,
    json: bool,
) -> anyhow::Result<()> {
    if follow {
        return attach_session(config, workspace_root, session_id, json).await;
    }
    print_log_file(config, workspace_root, session_id, json).await
}

async fn attach_session(
    config: &NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store =
        nca_runtime::session_store::SessionStore::new(resolve_sessions_dir(config, workspace_root));
    let session = store.load(session_id).await.map_err(anyhow::Error::msg)?;

    if let Some(socket_path) = session.meta.socket_path.clone() {
        let client = nca_runtime::ipc::IpcClient::new(socket_path);
        if let Ok(mut rx) = client.connect().await {
            while let Some(envelope) = rx.recv().await {
                print_event_envelope(&envelope, json)?;
            }
            return Ok(());
        }
    }

    print_log_file(config, workspace_root, session_id, json).await
}

async fn show_status(
    config: &NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store =
        nca_runtime::session_store::SessionStore::new(resolve_sessions_dir(config, workspace_root));
    let snapshot = store
        .load_snapshot(session_id)
        .await
        .map_err(anyhow::Error::msg)?;
    if json {
        print_json(&snapshot, false)?;
    } else {
        print_human_session(&snapshot);
    }
    Ok(())
}

async fn cancel_session(
    config: &NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store =
        nca_runtime::session_store::SessionStore::new(resolve_sessions_dir(config, workspace_root));
    let mut session = store.load(session_id).await.map_err(anyhow::Error::msg)?;
    let socket_path = session.meta.socket_path.clone();

    if let Some(socket_path) = session.meta.socket_path.clone() {
        let client = nca_runtime::ipc::IpcClient::new(socket_path);
        let _ = client.send_command(&AgentCommand::Shutdown).await;
    }

    if let Some(pid) = session.meta.pid {
        terminate_process(pid).await?;
    }

    #[cfg(unix)]
    if let Some(socket_path) = socket_path {
        match tokio::fs::remove_file(&socket_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to remove IPC endpoint {}: {error}",
                    socket_path.display()
                ));
            }
        }
    }

    session.meta.status = SessionStatus::Cancelled;
    session.meta.updated_at = chrono::Utc::now();
    session.meta.pid = None;
    session.meta.socket_path = None;
    store.save(&session).await.map_err(anyhow::Error::msg)?;
    if json {
        print_json(
            &CancelCommandOutput {
                session: session.snapshot(),
                cancelled: true,
            },
            false,
        )?;
    } else {
        println!("Cancelled {session_id}");
    }
    Ok(())
}

/// Terminate a detached session using the native process-control utility.
///
/// The IPC shutdown above is the graceful path. This fallback is needed when
/// the runtime is unresponsive, and must not invoke a Unix-only command on
/// Windows.
async fn terminate_process(pid: u32) -> anyhow::Result<()> {
    if pid == 0 {
        anyhow::bail!("invalid process id 0");
    }
    #[cfg(unix)]
    if pid > i32::MAX as u32 {
        anyhow::bail!("invalid process id {pid}");
    }

    #[cfg(unix)]
    let output = tokio::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .await?;

    #[cfg(windows)]
    let output = tokio::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()
        .await?;

    if output.status.success() {
        return Ok(());
    }

    let detail = command_output_detail(&output.stdout, &output.stderr);
    if process_was_already_gone(&detail) {
        return Ok(());
    }
    if detail.is_empty() {
        anyhow::bail!(
            "failed to terminate process {pid} (exit status {})",
            output.status
        );
    }
    anyhow::bail!("failed to terminate process {pid}: {detail}");
}

fn command_output_detail(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    format!("{stderr} {stdout}").trim().to_string()
}

fn process_was_already_gone(detail: &str) -> bool {
    let detail = detail.to_ascii_lowercase();
    detail.contains("no such process")
        || detail.contains("not found")
        || detail.contains("no running instance of the task")
}

async fn print_log_file(
    config: &NcaConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let log_path =
        resolve_sessions_dir(config, workspace_root).join(format!("{session_id}.events.jsonl"));
    let data = match tokio::fs::read_to_string(&log_path).await {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("No event log found for {session_id}");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    for line in data.lines() {
        let envelope: EventEnvelope = serde_json::from_str(line)?;
        print_event_envelope(&envelope, json)?;
    }
    Ok(())
}

fn print_human_session(session: &SessionSnapshot) {
    println!(
        "{}  status={:?}  model={}  updated={}  children={}",
        session.id,
        session.status,
        session.model,
        session.updated_at.to_rfc3339(),
        session.child_session_ids.len()
    );
    if let Some(summary) = &session.session_summary {
        println!("  summary: {}", summary.replace('\n', " "));
    }
}

fn print_event_envelope(envelope: &EventEnvelope, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(envelope)?);
    } else {
        stream::render_human_event(&envelope.event);
    }
    Ok(())
}

fn list_skills(config: &NcaConfig, workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    let skills = SkillCatalog::discover(workspace_root, &config.harness.skill_directories)
        .map_err(anyhow::Error::msg)?;
    if json {
        let output: Vec<_> = skills
            .into_iter()
            .map(|skill| {
                let source = skill.source_label().to_string();
                SkillOutput {
                    name: skill.name,
                    display_name: skill.display_name,
                    command: skill.command,
                    description: skill.description,
                    short_description: skill.short_description,
                    model: skill.model,
                    permission_mode: skill.permission_mode.map(|mode| format!("{mode:?}")),
                    context: format!("{:?}", skill.context),
                    source,
                    directory: skill.directory,
                    allow_implicit_invocation: skill.allow_implicit_invocation,
                }
            })
            .collect();
        print_json(&output, false)?;
    } else if skills.is_empty() {
        println!("No skills found");
    } else {
        for skill in skills {
            println!("{}", skill.summary_line());
        }
    }
    Ok(())
}

fn handle_skills_add(
    source: &str,
    skill_filter: &[String],
    global: bool,
    workspace_root: &Path,
) -> anyhow::Result<()> {
    use nca_core::skill_installer::{install_skills, parse_source};

    let parsed = parse_source(source).map_err(anyhow::Error::msg)?;
    let installed = install_skills(&parsed, skill_filter, global, workspace_root)
        .map_err(anyhow::Error::msg)?;

    let scope = if global { "(global)" } else { "(local)" };
    println!(
        "Installed {} skill(s) {scope}: {}",
        installed.len(),
        installed.join(", ")
    );
    Ok(())
}

fn handle_skills_remove(name: &str, global: bool, workspace_root: &Path) -> anyhow::Result<()> {
    use nca_core::skill_installer::remove_skill;

    remove_skill(name, global, workspace_root).map_err(anyhow::Error::msg)?;
    println!("Removed skill: {name}");
    Ok(())
}

fn handle_skills_update(name: Option<&str>, workspace_root: &Path) -> anyhow::Result<()> {
    use nca_core::skill_installer::{
        SkillLock, SkillLockEntry, copy_skill_dir, discover_skills_in_dir, git_clone_to_temp,
        git_head_commit, lock_file_path, skills_dir,
    };

    let mut updated = 0u32;
    let mut up_to_date = 0u32;
    let mut skipped = 0u32;

    for global in [false, true] {
        let lock_path = lock_file_path(global, workspace_root);
        let mut lock = SkillLock::load(&lock_path).map_err(anyhow::Error::msg)?;
        let target = skills_dir(global, workspace_root);
        let mut changed = false;

        let entries: Vec<_> = lock
            .skills
            .iter()
            .filter(|(k, _)| name.is_none() || name == Some(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (skill_name, entry) in entries {
            if entry.commit.is_none() {
                eprintln!("Skipping '{skill_name}' (installed from local path)");
                skipped += 1;
                continue;
            }

            let clone_url = entry
                .source
                .strip_prefix("github:")
                .map(|repo| format!("https://github.com/{repo}.git"));

            let Some(url) = clone_url else {
                eprintln!("Skipping '{skill_name}' (unknown source format)");
                skipped += 1;
                continue;
            };

            let tmp = git_clone_to_temp(&url).map_err(anyhow::Error::msg)?;
            let new_commit = git_head_commit(tmp.path()).ok();

            if new_commit.as_deref() == entry.commit.as_deref() {
                up_to_date += 1;
                continue;
            }

            let skills_path = if tmp.path().join("skills").is_dir() {
                tmp.path().join("skills")
            } else {
                tmp.path().to_path_buf()
            };

            let discovered = discover_skills_in_dir(&skills_path).map_err(anyhow::Error::msg)?;

            if let Some((_, src_dir)) = discovered.iter().find(|(n, _)| n == &skill_name) {
                let dest = target.join(&skill_name);
                copy_skill_dir(src_dir, &dest).map_err(anyhow::Error::msg)?;
                lock.upsert(
                    &skill_name,
                    SkillLockEntry {
                        source: entry.source.clone(),
                        commit: new_commit,
                        installed_at: chrono::Utc::now().to_rfc3339(),
                    },
                );
                changed = true;
                updated += 1;
            } else {
                eprintln!("Warning: skill '{skill_name}' no longer found in source repo");
                skipped += 1;
            }
        }

        if changed {
            lock.save(&lock_path).map_err(anyhow::Error::msg)?;
        }
    }

    println!("Updated {updated}, already up-to-date {up_to_date}, skipped {skipped}");
    Ok(())
}

fn list_mcp_servers(config: &NcaConfig, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(&config.mcp, false)?;
    } else if config.mcp.servers.is_empty() {
        println!("No MCP servers configured");
    } else {
        for server in config.mcp.servers.iter().filter(|server| server.enabled) {
            println!(
                "{}  command={} {}",
                server.name,
                server.command,
                server.args.join(" ")
            );
        }
    }
    Ok(())
}

async fn show_memory(config: &NcaConfig, workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    let store = workspace_memory_store(config, workspace_root);
    let state = store.load().await.map_err(anyhow::Error::msg)?;
    if json {
        print_json(&state, false)?;
    } else if state.notes.is_empty() {
        println!("No memory notes stored");
    } else {
        for note in state.notes {
            println!(
                "{}  {}  {}",
                note.id,
                note.kind,
                note.title.unwrap_or_else(|| note.created_at.to_rfc3339())
            );
            println!("  {}", note.content.replace('\n', " "));
        }
    }
    Ok(())
}

async fn add_memory_note(
    config: &NcaConfig,
    workspace_root: &Path,
    kind: &str,
    text: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store = workspace_memory_store(config, workspace_root);
    let note = MemoryNote {
        id: format!("{}-{}", kind, chrono::Utc::now().timestamp_millis()),
        created_at: chrono::Utc::now(),
        kind: kind.to_string(),
        title: None,
        content: text.trim().to_string(),
    };
    let state = store
        .append_note(note.clone(), config.memory.max_notes)
        .await
        .map_err(anyhow::Error::msg)?;
    if json {
        print_json(&note, false)?;
    } else {
        println!("Stored memory note {} ({})", note.id, kind);
        println!("Memory path: {}", store.path().display());
        println!("Total notes: {}", state.notes.len());
    }
    Ok(())
}

fn show_models(config: &NcaConfig, json: bool) -> anyhow::Result<()> {
    let output = ModelCatalogOutput {
        default_provider: config.provider.default.display_name().to_string(),
        default_model: config.model.default_model.clone(),
        provider_models: ProviderKind::ALL
            .into_iter()
            .map(|provider| ProviderModelOutput {
                provider: provider.display_name().to_string(),
                model: config.provider.model_for(provider).to_string(),
                base_url: config.provider.base_url_for(provider).to_string(),
                selected: provider == config.provider.default,
            })
            .collect(),
        aliases: config.model.aliases.clone(),
        thinking_enabled: config.model.enable_thinking,
        thinking_budget: config.model.thinking_budget,
        reasoning_effort: config.model.reasoning_effort.clone(),
        reasoning_effort_scope: "OpenAI-compatible only".into(),
        reasoning_effort_active: config.reasoning_effort_active_for_default_provider(),
    };
    if json {
        print_json(&output, false)?;
    } else {
        println!(
            "Default provider/model: {} / {}",
            output.default_provider, output.default_model
        );
        println!(
            "Thinking: {} (budget {})",
            if output.thinking_enabled { "on" } else { "off" },
            output.thinking_budget
        );
        println!(
            "Reasoning effort: {} ({}; {})",
            output.reasoning_effort,
            output.reasoning_effort_scope,
            if output.reasoning_effort_active {
                "active"
            } else {
                "inactive for active provider"
            }
        );
        println!("Provider models:");
        for provider in &output.provider_models {
            println!(
                "  {}{} -> {} ({})",
                provider.provider,
                if provider.selected { " [selected]" } else { "" },
                provider.model,
                provider.base_url
            );
        }
        for (alias, target) in output.aliases {
            println!("  {alias} -> {target}");
        }
    }
    Ok(())
}

fn show_doctor(config: &NcaConfig, workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    let skills = SkillCatalog::discover(workspace_root, &config.harness.skill_directories)
        .map(|skills| skills.len())
        .unwrap_or(0);
    let output = DoctorOutput {
        provider: config.provider.default.display_name().to_string(),
        default_model: config.model.default_model.clone(),
        providers: ProviderKind::ALL
            .into_iter()
            .map(|provider| ProviderDoctorStatus {
                provider: provider.display_name().to_string(),
                selected: provider == config.provider.default,
                api_key_present: config.provider.api_key_present_for(provider),
                api_key_env: config.provider.api_key_env_for(provider).to_string(),
                model: config.provider.model_for(provider).to_string(),
                base_url: config.provider.base_url_for(provider).to_string(),
            })
            .collect(),
        mcp_server_count: config
            .mcp
            .servers
            .iter()
            .filter(|server| server.enabled)
            .count(),
        skill_count: skills,
        memory_path: resolve_memory_path(config, workspace_root),
    };
    if json {
        print_json(&output, false)?;
    } else {
        println!("Provider: {}", output.provider);
        println!("Default model: {}", output.default_model);
        println!("Provider readiness:");
        for provider in &output.providers {
            println!(
                "  {}{}: api_key={} ({}) model={} base_url={}",
                provider.provider,
                if provider.selected { " [selected]" } else { "" },
                if provider.api_key_present {
                    "configured"
                } else {
                    "missing"
                },
                provider.api_key_env,
                provider.model,
                provider.base_url
            );
        }
        println!("Skills discovered: {}", output.skill_count);
        println!("MCP servers enabled: {}", output.mcp_server_count);
        println!("Memory path: {}", output.memory_path.display());
        println!("MiniMax remains the default recommended path for this workspace.");
    }
    Ok(())
}

async fn autoresearch_once(program: PathBuf, workspace: PathBuf) -> anyhow::Result<()> {
    use nca_autoresearch::experiment::{ExperimentConfig, ExperimentRunner};
    use nca_autoresearch::metric_parser::MetricParser;
    use nca_autoresearch::program::ResearchProgram;

    let prog = ResearchProgram::from_file(&program).map_err(|e| anyhow::anyhow!("{e}"))?;
    prog.validate()
        .map_err(|e| anyhow::anyhow!("invalid research program: {e}"))?;
    let shell_cmd = prog
        .metric_command
        .command
        .trim()
        .trim_matches('`')
        .trim()
        .replace('\n', " ");
    if shell_cmd.is_empty() {
        anyhow::bail!("research program has an empty metric cmd/command");
    }

    println!("workspace: {}", workspace.display());
    println!("program:   {}", program.display());
    println!("metric:    regex {:?}", prog.metric_command.parse_regex);
    println!("running:   sh -c {}", shell_cmd);

    let cfg = ExperimentConfig {
        working_dir: workspace,
        command: "sh".into(),
        args: vec!["-c".into(), shell_cmd.to_string()],
        time_budget_seconds: prog.time_budget_seconds,
        log_file: None,
        memory_limit_gb: prog.max_memory_gb,
        kill_timeout_factor: 2,
    };
    let runner = ExperimentRunner::new(cfg);
    let output = runner
        .run_with_description("nca autoresearch once".into())
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let parser = MetricParser::new();
    let metric = parser
        .extract_with_regex(&output.output, &prog.metric_command.parse_regex)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "could not parse metric with regex {:?} from output (first 800 chars):\n{}",
                prog.metric_command.parse_regex,
                output.output.chars().take(800).collect::<String>()
            )
        })?;

    println!("\n---");
    println!("metric value: {metric}");
    println!("experiment status: {:?}", output.status);
    println!("---");
    Ok(())
}

fn resolve_autoresearch_workspace(workspace: Option<PathBuf>, current: &Path) -> PathBuf {
    let workspace = workspace.unwrap_or_else(|| current.to_path_buf());
    if workspace.is_absolute() {
        workspace
    } else {
        current.join(workspace)
    }
}

fn autoresearch_start(
    program: PathBuf,
    workspace: PathBuf,
    session_id: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let store = nca_autoresearch::SessionStore::for_workspace(&workspace);
    let state = store.start(session_id.as_deref(), program, &workspace)?;
    print_autoresearch_state(&state, json)
}

fn autoresearch_status(
    workspace: PathBuf,
    session_id: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let store = nca_autoresearch::SessionStore::for_workspace(&workspace);
    let state = store.resolve(session_id.as_deref())?;
    print_autoresearch_state(&state, json)
}

fn autoresearch_stop(
    workspace: PathBuf,
    session_id: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let store = nca_autoresearch::SessionStore::for_workspace(&workspace);
    let current = store.resolve(session_id.as_deref())?;
    let state = store.stop(&current.session_id)?;
    print_autoresearch_state(&state, json)
}

fn autoresearch_resume(
    workspace: PathBuf,
    session_id: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let store = nca_autoresearch::SessionStore::for_workspace(&workspace);
    let current = store.resolve(session_id.as_deref())?;
    let state = store.resume(&current.session_id)?;
    print_autoresearch_state(&state, json)
}

fn autoresearch_results(
    workspace: PathBuf,
    session_id: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let store = nca_autoresearch::SessionStore::for_workspace(&workspace);
    let state = store.resolve(session_id.as_deref())?;
    let results = store.results(&state.session_id)?;
    if json {
        print_json(
            &AutoresearchResultsOutput {
                session: state,
                results,
            },
            false,
        )
    } else {
        print_autoresearch_state(&state, false)?;
        for result in results {
            println!(
                "{}\tmetric={}\tstatus={}\tdescription={}",
                result.timestamp.to_rfc3339(),
                result.metric_value,
                result.status,
                result.description
            );
        }
        Ok(())
    }
}

fn print_autoresearch_state(
    state: &nca_autoresearch::SessionState,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        print_json(state, false)?;
    } else {
        let baseline = state
            .baseline_metric
            .map(|value| value.to_string())
            .unwrap_or_else(|| "none".to_string());
        let best = state
            .best_result
            .as_ref()
            .map(|result| result.metric_value.to_string())
            .unwrap_or_else(|| "none".to_string());
        println!(
            "{} status={} program={} workspace={} iterations={} baseline={} best={}",
            state.session_id,
            state.status,
            state.program_name,
            state.workspace.display(),
            state.iteration_count,
            baseline,
            best
        );
    }
    Ok(())
}

fn show_config(config: &NcaConfig, workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    if json {
        print_json(
            &ConfigOutput {
                config,
                reasoning_effort: &config.model.reasoning_effort,
                reasoning_effort_scope: "OpenAI-compatible only",
                reasoning_effort_active: config.reasoning_effort_active_for_default_provider(),
            },
            false,
        )?;
    } else {
        println!(
            "Global config: {}",
            nca_common::config::global_config_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "<unavailable>".into())
        );
        println!(
            "Workspace config: {}",
            nca_common::config::workspace_config_path(workspace_root).display()
        );
        println!("Default provider: {:?}", config.provider.default);
        println!("Default model: {}", config.model.default_model);
        println!(
            "Reasoning effort: {} (OpenAI-compatible only; {})",
            config.model.reasoning_effort,
            if config.reasoning_effort_active_for_default_provider() {
                "active"
            } else {
                "inactive for active provider"
            }
        );
        println!("Permission mode: {:?}", config.permissions.mode);
        println!("Provider endpoints:");
        for provider in ProviderKind::ALL {
            println!(
                "  {} -> model={} base_url={}",
                provider.display_name(),
                config.provider.model_for(provider),
                config.provider.base_url_for(provider)
            );
        }
        println!(
            "Memory path: {}",
            workspace_memory_store(config, workspace_root)
                .path()
                .display()
        );
        println!("Skill directories:");
        for path in &config.harness.skill_directories {
            let resolved = if path.is_absolute() {
                path.clone()
            } else {
                workspace_root.join(path)
            };
            println!("  {}", resolved.display());
        }
    }
    Ok(())
}

fn workspace_memory_store(config: &NcaConfig, workspace_root: &Path) -> MemoryStore {
    MemoryStore::new(resolve_memory_path(config, workspace_root))
}

fn reject_safe_yolo(safe: bool, yolo: bool) -> anyhow::Result<()> {
    if safe && yolo {
        anyhow::bail!("--safe and --yolo cannot be used together");
    }
    if yolo {
        eprintln!(
            "[warning] --yolo disables nca-level approval and safety guards for this invocation; OS permissions and outer sandboxes still apply"
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct RunCommandOutput {
    session: SessionSnapshot,
    output: String,
    end_reason: &'static str,
}

#[derive(serde::Serialize)]
struct SpawnCommandOutput {
    session_id: String,
    pid: u32,
    status_path: PathBuf,
    event_log_path: PathBuf,
    spawn_log_path: PathBuf,
    socket_path: PathBuf,
    permission_mode: String,
    safe_mode: bool,
    yolo: bool,
}

#[derive(serde::Serialize)]
struct SessionListOutput {
    sessions: Vec<SessionSnapshot>,
    unreadable: Vec<String>,
}

#[derive(serde::Serialize)]
struct CancelCommandOutput {
    session: SessionSnapshot,
    cancelled: bool,
}

#[derive(serde::Serialize)]
struct AutoresearchResultsOutput {
    session: nca_autoresearch::SessionState,
    results: Vec<nca_autoresearch::ExperimentResult>,
}

#[derive(serde::Serialize)]
struct SkillOutput {
    name: String,
    display_name: Option<String>,
    command: String,
    description: Option<String>,
    short_description: Option<String>,
    model: Option<String>,
    permission_mode: Option<String>,
    context: String,
    source: String,
    directory: PathBuf,
    allow_implicit_invocation: bool,
}

#[derive(serde::Serialize)]
struct ModelCatalogOutput {
    default_provider: String,
    default_model: String,
    provider_models: Vec<ProviderModelOutput>,
    aliases: std::collections::BTreeMap<String, String>,
    thinking_enabled: bool,
    thinking_budget: u32,
    reasoning_effort: String,
    reasoning_effort_scope: String,
    reasoning_effort_active: bool,
}

#[derive(serde::Serialize)]
struct ConfigOutput<'a> {
    #[serde(flatten)]
    config: &'a NcaConfig,
    reasoning_effort: &'a str,
    reasoning_effort_scope: &'static str,
    reasoning_effort_active: bool,
}

#[derive(serde::Serialize)]
struct DoctorOutput {
    provider: String,
    default_model: String,
    providers: Vec<ProviderDoctorStatus>,
    mcp_server_count: usize,
    skill_count: usize,
    memory_path: PathBuf,
}

#[derive(serde::Serialize)]
struct ProviderModelOutput {
    provider: String,
    model: String,
    base_url: String,
    selected: bool,
}

#[derive(serde::Serialize)]
struct ProviderDoctorStatus {
    provider: String,
    selected: bool,
    api_key_present: bool,
    api_key_env: String,
    model: String,
    base_url: String,
}

fn print_json<T: serde::Serialize>(value: &T, pretty: bool) -> anyhow::Result<()> {
    let rendered = if pretty {
        serde_json::to_string_pretty(value)?
    } else {
        serde_json::to_string(value)?
    };
    println!("{rendered}");
    Ok(())
}

fn generate_shell_completion(shell: ClapShell) {
    let mut cmd = Cli::command();
    let bin_name = "nca";

    match shell {
        ClapShell::Bash => {
            generate(
                clap_complete::shells::Bash,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Zsh => {
            generate(
                clap_complete::shells::Zsh,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Fish => {
            generate(
                clap_complete::shells::Fish,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::PowerShell => {
            generate(
                clap_complete::shells::PowerShell,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
        ClapShell::Elvish => {
            generate(
                clap_complete::shells::Elvish,
                &mut cmd,
                bin_name,
                &mut std::io::stdout(),
            );
        }
    }
}

fn classify_exit_code(error: &anyhow::Error) -> ExitCode {
    const EXIT_CONFIGURATION: u8 = 10;
    const EXIT_RUNTIME: u8 = 11;
    const EXIT_APPROVAL: u8 = 13;
    const EXIT_CANCELLED: u8 = 130;

    let mut combined = String::new();
    for (idx, cause) in error.chain().enumerate() {
        if idx > 0 {
            combined.push_str(" | ");
        }
        combined.push_str(&cause.to_string().to_ascii_lowercase());
    }

    let code = if combined.contains("requires approval in headless mode")
        || combined.contains("requires approval; request was denied")
        || combined.contains("denied by policy")
    {
        EXIT_APPROVAL
    } else if combined.contains("run cancelled") {
        EXIT_CANCELLED
    } else if combined.contains("missing minimax api key")
        || combined.contains("failed to parse config file")
        || combined.contains("unable to determine the home directory")
        || combined.contains("invalid workspace root")
    {
        EXIT_CONFIGURATION
    } else if combined.contains("provider")
        || combined.contains("tool `")
        || combined.contains("empty response")
        || combined.contains("turn budget exceeded")
    {
        EXIT_RUNTIME
    } else {
        1
    };

    ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_top_level_run_mode() {
        let cli = Cli::try_parse_from(["nca", "--run"]).expect("should parse run mode");
        assert!(cli.run);
        assert!(cli.command.is_none());
    }

    #[test]
    fn parses_run_subcommand_model_override() {
        let cli =
            Cli::try_parse_from(["nca", "run", "--prompt", "hello", "--model", "MiniMax-M2.5"])
                .expect("should parse run subcommand");

        match cli.command {
            Some(Command::Run { model, .. }) => {
                assert_eq!(model.as_deref(), Some("MiniMax-M2.5"));
            }
            _ => panic!("expected run subcommand"),
        }
    }

    #[test]
    fn parses_reasoning_effort_after_run_subcommand() {
        let cli = Cli::try_parse_from([
            "nca",
            "run",
            "--prompt",
            "hello",
            "--reasoning-effort",
            "high",
        ])
        .expect("should parse reasoning effort after run subcommand");

        assert_eq!(cli.reasoning_effort.as_deref(), Some("high"));
    }

    #[test]
    fn parses_yolo_before_and_after_subcommands() {
        for args in [
            vec!["nca", "--yolo", "run", "--prompt", "hello"],
            vec!["nca", "run", "--prompt", "hello", "--yolo"],
        ] {
            let cli = Cli::try_parse_from(args).expect("should parse yolo placement");
            assert!(cli.yolo);
        }
    }

    #[test]
    fn rejects_safe_and_yolo() {
        assert!(reject_safe_yolo(true, true).is_err());
        assert!(reject_safe_yolo(false, true).is_ok());
    }

    #[test]
    fn spawn_args_forward_reasoning_effort_before_subcommand() {
        let args = spawn_command_args(
            "hello",
            "session-1",
            Some("high"),
            Some("gpt-5"),
            true,
            CliPermissionMode::AcceptEdits,
            false,
        );

        assert_eq!(
            args,
            vec![
                "--reasoning-effort",
                "high",
                "run",
                "--prompt",
                "hello",
                "--stream",
                "ndjson",
                "--session-id",
                "session-1",
                "--permission-mode",
                "accept-edits",
                "--safe",
                "--model",
                "gpt-5",
            ]
        );
    }

    #[tokio::test]
    async fn terminates_a_child_with_native_process_control() {
        #[cfg(unix)]
        let mut child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep should be available on Unix");

        #[cfg(windows)]
        let mut child = tokio::process::Command::new("cmd.exe")
            .args(["/D", "/C", "ping 127.0.0.1 -n 31 >NUL"])
            .spawn()
            .expect("cmd.exe should be available on Windows");

        terminate_process(child.id().expect("child should have a pid"))
            .await
            .expect("native process termination should succeed");
        let status = child.wait().await.expect("child should be reaped");
        assert!(!status.success());
    }

    #[tokio::test]
    async fn terminate_process_reports_invalid_pid() {
        let error = terminate_process(0)
            .await
            .expect_err("zero is not a valid process id");
        assert!(error.to_string().contains("invalid process id 0"));
    }

    #[test]
    fn recognizes_windows_taskkill_already_exited_wording() {
        assert!(process_was_already_gone(
            "ERROR: The process with PID 1234 could not be terminated.\nReason: There is no running instance of the task."
        ));
    }
}
