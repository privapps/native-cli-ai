use crate::file_mentions::{
    at_token_before_cursor, discover_workspace_files, expand_at_file_mentions_default,
    filter_paths_prefix,
};
use crate::prompt::NcaPrompt;
use crate::runner::{SessionRuntime, dispatch_question_answer, dispatch_tool_approval};
use crate::slash_commands::{help_lines as registry_help_lines, visible_commands};
use crate::tui::custom_provider_flow::{CustomProviderProbeOutcome, CustomProviderSetupTransition};
use crate::tui::{
    ApprovalAnswer, CustomProviderProbeAction, CustomProviderSetupSubmission, DisplayBlock,
    ModelPickerAction, ModelPickerEntry, SharedTuiState, TuiCmd, TuiSessionState,
    git_create_branch, git_current_branch, git_list_branches, git_switch_branch,
    replay_event_log_into_state, run_blocking, spawn_tui_bridge,
};
use nca_common::config::{
    CustomCredentialSource, CustomProviderConfig, CustomProviderConfigError, CustomProviderSetup,
    NcaConfig, PermissionMode, ProviderCompatibility, ProviderConfigPatch, ProviderKind,
    normalize_custom_provider_base_url, validate_custom_api_key_env_name,
};
use nca_common::event::{EndReason, QuestionSelection};
use nca_core::skills::SkillCatalog;
use nca_runtime::memory_store::MemoryStore;
use reedline::{Completer, Emacs, FileBackedHistory, Reedline, Signal, Suggestion, Vi};
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

/// Where slash-command and preset output goes (TTY transcript vs full-screen TUI).
pub(crate) enum ReplOutput<'a> {
    Stdio,
    Tui(&'a Arc<Mutex<TuiSessionState>>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CustomProviderCommand {
    compatibility: ProviderCompatibility,
    base_url: String,
    api_key: Option<String>,
    model: Option<String>,
}

fn parse_custom_provider_command(
    rest: &str,
) -> Result<Option<CustomProviderCommand>, &'static str> {
    let mut tokens = rest.split_whitespace();
    let Some(compatibility) = tokens.next() else {
        return Ok(None);
    };
    let Some(base_url) = tokens.next() else {
        return Ok(None);
    };
    let compatibility = ProviderCompatibility::from_cli_name(compatibility)
        .ok_or("compatibility must be `openai`, `responses`, or `anthropic`")?;
    Ok(Some(CustomProviderCommand {
        compatibility,
        base_url: base_url.to_string(),
        api_key: tokens.next().map(str::to_string),
        model: tokens.next().map(str::to_string),
    }))
}

impl ReplOutput<'_> {
    fn print(&self, s: &str) {
        match self {
            ReplOutput::Stdio => {
                print!("{s}");
                let _ = std::io::stdout().flush();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    for line in s.split('\n') {
                        g.blocks.push(DisplayBlock::System(line.to_string()));
                    }
                    g.mark_transcript_dirty();
                }
            }
        }
    }

    fn println(&self, s: &str) {
        self.print(&format!("{s}\n"));
    }

    fn eprintln(&self, s: &str) {
        match self {
            ReplOutput::Stdio => eprintln!("{s}"),
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.push(DisplayBlock::System(format!("[!] {s}")));
                    g.mark_transcript_dirty();
                }
            }
        }
    }

    fn clear_screen(&self) {
        match self {
            ReplOutput::Stdio => {
                print!("\x1B[2J\x1B[H");
                std::io::stdout().flush().ok();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.clear();
                    g.streaming_assistant = None;
                    g.scroll_lines = 0;
                    g.transcript_follow_tail = true;
                    g.mark_transcript_dirty();
                }
            }
        }
    }
}

/// Special input prefixes
#[allow(dead_code)]
const INPUT_PREFIXES: &[&str] = &[
    "!",  // Bash mode - run shell command directly
    "@",  // File reference - fuzzy file search
    "\\", // Multiline continuation
];

/// Agent profiles inspired by OpenCode's multi-agent system.
/// Each profile modifies behavior and system prompt emphasis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentProfile {
    /// Default full-access agent for development work
    #[default]
    Build,
    /// Read-only agent for analysis and planning - denies edits
    Plan,
    /// Focused code review agent
    Review,
    /// Bug diagnosis and fix agent
    Fix,
    /// Testing and validation agent
    Test,
}

impl AgentProfile {
    /// Get the display name for this profile (shown in prompt)
    pub fn label(&self) -> &'static str {
        match self {
            AgentProfile::Build => "build",
            AgentProfile::Plan => "plan",
            AgentProfile::Review => "review",
            AgentProfile::Fix => "fix",
            AgentProfile::Test => "test",
        }
    }

    /// Get system prompt modifier for this profile
    #[allow(dead_code)]
    pub fn system_modifier(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => {
                "Profile: PLAN MODE (read-only)\n- You must not modify files or run shell commands.\n\
                 - Inspect, search, read, research the web, and propose the next steps only.\n\
                 - If asked to change code, explain what would change instead of claiming it was done."
            }
            AgentProfile::Review => {
                "Profile: REVIEW MODE\n- Focus on identifying bugs, regressions, security issues, and code quality problems.\n\
                 - Check for missing tests, edge cases, and error handling.\n\
                 - Be specific about severity: critical, major, minor, or suggestion."
            }
            AgentProfile::Fix => {
                "Profile: FIX MODE\n- Diagnose the issue thoroughly before making changes.\n\
                 - Prefer minimal, verified fixes over broad rewrites.\n\
                 - Always explain the root cause and the fix."
            }
            AgentProfile::Test => {
                "Profile: TEST MODE\n- Focus on validating code correctness and edge cases.\n\
                 - Run tests, checks, or lints when tools allow.\n\
                 - Report clearly what passed, what failed, and any issues found."
            }
        }
    }

    /// Get reedline suggestion color for this profile
    #[allow(dead_code)]
    pub fn style(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => "cyan",
            AgentProfile::Review => "yellow",
            AgentProfile::Fix => "red",
            AgentProfile::Test => "green",
        }
    }

    /// Cycle to the next profile (for Tab switching)
    pub fn next(self) -> Self {
        match self {
            AgentProfile::Build => AgentProfile::Plan,
            AgentProfile::Plan => AgentProfile::Review,
            AgentProfile::Review => AgentProfile::Fix,
            AgentProfile::Fix => AgentProfile::Test,
            AgentProfile::Test => AgentProfile::Build,
        }
    }

    /// All profiles in cycle order
    pub const ALL: [Self; 5] = [Self::Build, Self::Plan, Self::Review, Self::Fix, Self::Test];
}

impl std::fmt::Display for AgentProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Session state for REPL
pub struct Repl {
    runtime: SessionRuntime,
    prompt: NcaPrompt,
    run_mode: bool,
    history_path: std::path::PathBuf,
    agent_profile: AgentProfile,
    current_agent_label: String,
    pending_custom_setup: Option<CustomProviderSetupSubmission>,
}

impl Repl {
    pub fn new(runtime: SessionRuntime, safe_mode: bool, run_mode: bool) -> Self {
        let yolo = runtime.execution_context().yolo;
        let history_path = runtime.workspace_root().join(".nca/.history");
        let agent_profile = AgentProfile::default();
        let current_agent_label = format!("@{}", agent_profile.label());
        Self {
            runtime,
            prompt: NcaPrompt::new(safe_mode, run_mode, yolo),
            run_mode,
            history_path,
            agent_profile,
            current_agent_label,
            pending_custom_setup: None,
        }
    }

    fn custom_setup_required(&self) -> bool {
        Self::custom_provider_setup_required(self.runtime.config())
    }

    fn custom_provider_setup_required(config: &NcaConfig) -> bool {
        let custom = &config.provider.custom;
        normalize_custom_provider_base_url(&custom.base_url).is_err()
            || validate_custom_api_key_env_name(&custom.api_key_env).is_err()
            || custom.model.trim().is_empty()
            || custom.resolve_api_key().is_none()
    }

    /// Run the interactive REPL until the user exits.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        let mut editor = self.build_editor()?;

        let _spawn_task = {
            let spawn_rx = self.runtime.take_spawn_rx();
            let event_tx = self.runtime.event_tx();
            if let Some(srx) = spawn_rx {
                Some(nca_runtime::supervisor::spawn_subagent_consumer(
                    srx,
                    self.runtime.session_id().to_string(),
                    self.runtime.workspace_root().to_path_buf(),
                    self.runtime.config().clone(),
                    self.runtime.execution_context(),
                    self.runtime.safe_mode(),
                    self.runtime.messages().to_vec(),
                    event_tx,
                ))
            } else {
                None
            }
        };

        if self.run_mode {
            self.print_banner();
        }

        loop {
            // Update prompt with current agent profile
            self.prompt.set_agent(&self.current_agent_label);
            let sig = editor.read_line(&self.prompt);
            match sig {
                Ok(Signal::Success(input)) => {
                    if input.is_empty() {
                        continue;
                    }

                    // Tab switches agent profile (OpenCode-style)
                    if input == "\t" {
                        self.switch_agent();
                        continue;
                    }

                    // Bash mode: ! prefix runs shell command directly
                    if input.starts_with('!') {
                        let cmd = input.trim_start_matches('!');
                        self.run_bash_command(cmd).await;
                        continue;
                    }

                    // Slash commands
                    if input.starts_with('/') {
                        if !self.handle_command(&input, ReplOutput::Stdio).await? {
                            break;
                        }
                        continue;
                    }

                    let expanded = match expand_at_file_mentions_default(
                        &input,
                        self.runtime.workspace_root(),
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("file mention expansion: {e}");
                            continue;
                        }
                    };
                    match self.runtime.run_turn(&expanded).await {
                        Ok(output) => {
                            println!("{output}");
                        }
                        Err(err) => {
                            eprintln!("error: {err}");
                        }
                    }
                }
                Ok(Signal::CtrlD) => {
                    // Ctrl+D - exit
                    eprintln!("\n[exit]");
                    break;
                }
                Ok(Signal::CtrlC) => {
                    // Ctrl+C - cancel current or exit
                    eprintln!(
                        "\n[cancel] Press Ctrl+D to exit, or wait for current operation to complete"
                    );
                }
                Err(err) => {
                    eprintln!("read error: {err}");
                    break;
                }
            }
        }

        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    fn print_banner(&self) {
        eprintln!(
            r#"
╔══════════════════════════════════════════════════════════════╗
║  nca - Native CLI AI                                          ║
║  Interactive terminal mode                                     ║
╠══════════════════════════════════════════════════════════════╣
║  Shortcuts:                                                   ║
║    ! <cmd>   Run shell command (bash mode)                    ║
║    @path     Inline file mentions (expanded before send)      ║
║    / <cmd>   Slash commands                                  ║
║    Tab       Switch agent profile (@build/@plan/@review...)   ║
║    Ctrl+D    Exit                                            ║
║    Ctrl+C    Cancel current request                           ║
║    Ctrl+L    Clear screen                                     ║
║    Ctrl+R    Search command history                           ║
╚══════════════════════════════════════════════════════════════╝
"#
        );
    }

    /// Switch to the next agent profile (called on Tab press)
    fn switch_agent(&mut self) {
        let next = self.agent_profile.next();
        self.agent_profile = next;
        self.current_agent_label = format!("@{}", next.label());
        self.prompt.set_agent(&self.current_agent_label);

        // Update runtime permission mode based on profile
        if next == AgentProfile::Plan {
            self.runtime.set_permission_mode(PermissionMode::Plan);
        }

        eprintln!("\n[agent] Switched to @{} mode", next.label());
        if next == AgentProfile::Plan {
            eprintln!("[agent] Plan mode: file edits and shell commands are disabled");
        }
    }

    /// Run a shell command directly (bash mode) - Claude Code style
    /// Output is returned to the conversation context
    async fn run_bash_command(&self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            eprintln!("! usage: !<command> [args]");
            return;
        }

        eprintln!("[bash] {cmd}");

        let output = self.runtime.run_direct_bash(cmd).await;

        match output {
            Ok(output) => {
                if !output.is_empty() {
                    println!("{output}");
                }
            }
            Err(e) => eprintln!("[bash] failed to execute: {e}"),
        }
    }

    /// Open the configured external editor (`NCA_EDITOR`, `[ui].editor`, `EDITOR`, `vim`).
    async fn open_external_editor(&self, seed: Option<&str>) -> Option<String> {
        let editor_cmd = self.runtime.config().effective_editor_command();
        let temp_file = format!("nca-prompt-{}.txt", std::process::id());
        let temp_path = std::env::temp_dir().join(&temp_file);
        std::fs::write(&temp_path, seed.unwrap_or("")).ok()?;

        let output = Command::new("sh")
            .arg("-c")
            .arg(format!("{} '{}'", editor_cmd, temp_path.display()))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(_) => {
                let content = std::fs::read_to_string(&temp_path).ok()?;
                let _ = std::fs::remove_file(&temp_path);
                let content = content.trim().to_string();
                if content.is_empty() {
                    None
                } else {
                    Some(content)
                }
            }
            Err(e) => {
                eprintln!("[editor] Failed to open: {e}");
                None
            }
        }
    }

    async fn apply_provider_in_session(
        &mut self,
        p: ProviderKind,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        if p == ProviderKind::Custom && self.custom_setup_required() {
            if let ReplOutput::Tui(st) = &out {
                if let Ok(mut g) = st.lock() {
                    g.open_custom_provider_setup_from_config(
                        &self.runtime.config().provider.custom,
                    );
                    g.blocks.push(DisplayBlock::System(
                        "[provider] configure the custom endpoint to continue".into(),
                    ));
                    g.mark_transcript_dirty();
                }
            } else {
                out.eprintln(
                    "[provider] custom provider is not configured yet; use /custom <openai|anthropic> <base-url> [api-key] [model]",
                );
            }
            return Ok(());
        }
        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(p);
        match self.runtime.apply_nca_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                    g.set_active_provider(p, self.runtime.config().provider.base_url_for(p));
                    g.mark_dirty();
                }
                match self.runtime.config().save_provider_patch_workspace(
                    self.runtime.workspace_root(),
                    &ProviderConfigPatch::activate(p),
                ) {
                    Ok(()) => out.println(&format!(
                        "[provider] {} — model {} — saved .nca/config.local.toml",
                        p.display_name(),
                        self.runtime.model()
                    )),
                    Err(e) => out.eprintln(&format!(
                        "[provider] applied but workspace save failed: {e}"
                    )),
                }
            }
            Err(e) => out.eprintln(&format!("[provider] {e}")),
        }
        Ok(())
    }

    /// Save custom provider fields, switch default to Custom, and persist workspace config.
    async fn persist_custom_provider_config(
        &mut self,
        compatibility: ProviderCompatibility,
        base_url: String,
        api_key_env: String,
        api_key: Option<String>,
        model: Option<String>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<Option<String>> {
        let mut cfg = self.runtime.config().clone();
        let api_key = api_key.filter(|k| !k.trim().is_empty());
        let model_override = model.filter(|m| !m.trim().is_empty());
        let setup = CustomProviderSetup {
            compatibility,
            base_url,
            api_key_env,
            credential: api_key
                .clone()
                .map(CustomCredentialSource::Inline)
                .unwrap_or(CustomCredentialSource::Preserve),
            model: model_override
                .clone()
                .unwrap_or_else(|| cfg.provider.custom.model.clone()),
        };
        if let Err(e) = cfg.provider.custom.apply_setup(setup) {
            out.eprintln(&format!("[custom] {e}"));
            return Err(anyhow::anyhow!(
                "custom provider configuration rejected: {e}"
            ));
        }
        let base_url = cfg.provider.custom.base_url.clone();
        let patch = ProviderConfigPatch {
            provider: ProviderKind::Custom,
            set_default_provider: true,
            api_key_env: Some(cfg.provider.custom.api_key_env.clone()),
            api_key: api_key.clone(),
            base_url: Some(base_url.clone()),
            model: model_override,
            temperature: None,
            compatibility: Some(compatibility),
            onboarding_completed: None,
        };
        cfg.set_default_provider(ProviderKind::Custom);

        let activation = self
            .runtime
            .apply_nca_config(cfg)
            .map_err(|error| error.to_string());
        let activation_error = activation.as_ref().err().cloned();
        let persistence = if activation.is_ok() {
            self.runtime
                .config()
                .save_provider_patch_workspace(self.runtime.workspace_root(), &patch)
                .map_err(|error| error.to_string())
        } else {
            Ok(())
        };
        let outcome = crate::tui::input::provider_activation_outcome(activation, persistence);
        if !outcome.active {
            let error =
                activation_error.unwrap_or_else(|| "unknown provider activation failure".into());
            out.eprintln(&format!("[custom] {error}"));
            return Err(anyhow::anyhow!(
                "custom provider activation failed: {error}"
            ));
        }

        let persistence_warning = outcome.warning.map(|warning| {
            out.eprintln(&format!("[custom] applied but {warning}"));
            warning
        });
        if persistence_warning.is_none() {
            out.println(&format!(
                "[custom] {} at {} — model {} — saved .nca/config.local.toml",
                compatibility.display_name(),
                self.runtime.config().provider.custom.base_url,
                self.runtime.model()
            ));
        }
        if let ReplOutput::Tui(st) = out
            && let Ok(mut g) = st.lock()
        {
            g.model = self.runtime.model().to_string();
            g.set_active_provider(
                ProviderKind::Custom,
                self.runtime.config().provider.custom.base_url.as_str(),
            );
            g.mark_dirty();
        }
        Ok(persistence_warning)
    }

    fn custom_config_from_submission(
        &self,
        submission: &CustomProviderSetupSubmission,
    ) -> Result<CustomProviderConfig, CustomProviderConfigError> {
        let mut custom = self.runtime.config().provider.custom.clone();
        custom.apply_setup(CustomProviderSetup {
            compatibility: submission.compatibility,
            base_url: submission.base_url.clone(),
            api_key_env: submission.api_key_env.clone(),
            credential: submission
                .api_key
                .clone()
                .map(CustomCredentialSource::Inline)
                .unwrap_or(CustomCredentialSource::Preserve),
            model: submission.model.clone(),
        })?;
        Ok(custom)
    }

    async fn finish_custom_setup_submission(
        &mut self,
        submission: CustomProviderSetupSubmission,
        tui_state: &Arc<Mutex<TuiSessionState>>,
        probe: bool,
    ) -> anyhow::Result<()> {
        let custom = match self.custom_config_from_submission(&submission) {
            Ok(custom) => custom,
            Err(error) => {
                if let Ok(mut g) = tui_state.lock() {
                    let step = match &error {
                        CustomProviderConfigError::InvalidBaseUrl => {
                            crate::tui::state::CustomProviderSetupStep::BaseUrl
                        }
                        CustomProviderConfigError::InvalidApiKeyEnvironmentName
                        | CustomProviderConfigError::MissingApiKey => {
                            crate::tui::state::CustomProviderSetupStep::ApiKey
                        }
                    };
                    g.open_custom_provider_setup_for_values(
                        submission.compatibility,
                        submission.base_url.clone(),
                        submission.api_key_env.clone(),
                        submission.model.clone(),
                        step,
                    );
                    g.push_error(format!("[custom] {error}"));
                }
                return Ok(());
            }
        };
        if custom.resolve_api_key().is_none() {
            if let Ok(mut g) = tui_state.lock() {
                g.open_custom_provider_setup_for_values(
                    submission.compatibility,
                    submission.base_url.clone(),
                    submission.api_key_env.clone(),
                    submission.model.clone(),
                    crate::tui::state::CustomProviderSetupStep::ApiKey,
                );
                g.push_error(format!(
                    "[custom] missing API key; set {} or enter an inline secret",
                    custom.api_key_env
                ));
            }
            return Ok(());
        }

        if probe {
            let outcome = match nca_core::provider::custom::probe_custom_provider(&custom).await {
                Ok(()) => CustomProviderProbeOutcome::Succeeded,
                Err(nca_core::provider::ProviderError::Configuration(message)) => {
                    CustomProviderProbeOutcome::HardFailure(message)
                }
                Err(error) => CustomProviderProbeOutcome::RetryableFailure(error.to_string()),
            };
            let transition = if let Ok(mut g) = tui_state.lock() {
                g.apply_custom_provider_probe_outcome(outcome)
            } else {
                CustomProviderSetupTransition::Blocked(
                    "custom provider setup state is unavailable".into(),
                )
            };
            match transition {
                CustomProviderSetupTransition::Completed(_) => {}
                CustomProviderSetupTransition::ContinueWithMessage(_) => {
                    self.pending_custom_setup = Some(submission);
                    return Ok(());
                }
                CustomProviderSetupTransition::Blocked(message) => {
                    self.pending_custom_setup = None;
                    if let Ok(mut g) = tui_state.lock() {
                        g.push_error(format!("[custom] {message}"));
                    }
                    return Ok(());
                }
                _ => {
                    self.pending_custom_setup = None;
                    return Ok(());
                }
            }
        }

        let persistence_warning = match self
            .persist_custom_provider_config(
                submission.compatibility,
                submission.base_url.clone(),
                submission.api_key_env.clone(),
                submission.api_key.clone(),
                Some(submission.model.clone()),
                ReplOutput::Tui(tui_state),
            )
            .await
        {
            Ok(warning) => warning,
            Err(error) => {
                self.pending_custom_setup = None;
                if let Ok(mut g) = tui_state.lock() {
                    g.open_custom_provider_setup_for_values(
                        submission.compatibility,
                        submission.base_url,
                        submission.api_key_env,
                        submission.model,
                        crate::tui::state::CustomProviderSetupStep::Model,
                    );
                    g.push_error(format!("[custom] {error}"));
                }
                return Ok(());
            }
        };
        self.pending_custom_setup = None;
        if let Ok(mut g) = tui_state.lock() {
            g.close_custom_provider_setup();
            g.set_active_provider(
                ProviderKind::Custom,
                self.runtime.config().provider.custom.base_url.as_str(),
            );
            let message = if let Some(warning) = persistence_warning {
                format!("[custom] provider active; {warning}")
            } else {
                "[custom] provider saved and set as default".into()
            };
            g.blocks.push(DisplayBlock::System(message));
            g.mark_transcript_dirty();
        }
        Ok(())
    }

    async fn save_provider_api_key(
        &mut self,
        p: ProviderKind,
        key: &str,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let patch = ProviderConfigPatch {
            provider: p,
            set_default_provider: false,
            api_key_env: None,
            api_key: Some(key.to_string()),
            base_url: None,
            model: None,
            temperature: None,
            compatibility: None,
            onboarding_completed: None,
        };
        let mut cfg = self.runtime.config().clone();
        cfg.set_provider_api_key(p, key);
        match self.runtime.apply_nca_config(cfg) {
            Ok(()) => {
                if let Err(e) = self
                    .runtime
                    .config()
                    .save_provider_patch_workspace(self.runtime.workspace_root(), &patch)
                {
                    out.eprintln(&format!("[apikey] applied but workspace save failed: {e}"));
                } else {
                    out.println(&format!("[apikey] saved for {}", p.display_name()));
                }
            }
            Err(e) => out.eprintln(&format!("[apikey] {e}")),
        }
        Ok(())
    }

    fn build_editor(&self) -> anyhow::Result<Reedline> {
        let mut builder = Reedline::create()
            .with_quick_completions(true)
            .with_partial_completions(true)
            .with_ansi_colors(true);

        // Try to load history from disk
        if let Some(parent) = self.history_path.parent() {
            std::fs::create_dir_all(parent).ok();
            if let Ok(history) = FileBackedHistory::with_file(100, self.history_path.clone()) {
                builder = builder.with_history(Box::new(history));
            }
        }

        // Support vim mode if enabled via env
        if std::env::var("NCA_EDITOR_MODE")
            .map(|v| v.eq_ignore_ascii_case("vi") || v.eq_ignore_ascii_case("vim"))
            .unwrap_or(false)
        {
            builder = builder.with_edit_mode(Box::new(Vi::default()));
        } else {
            builder = builder.with_edit_mode(Box::new(Emacs::default()));
        }

        Ok(builder)
    }

    async fn handle_command(&mut self, input: &str, out: ReplOutput<'_>) -> anyhow::Result<bool> {
        let mut parts = input.split_whitespace();
        let raw_command = parts.next().unwrap_or_default();
        let rest = input
            .strip_prefix(raw_command)
            .map(str::trim)
            .unwrap_or_default();
        let command = match raw_command {
            "/models" => "/model",
            "/stats" | "/cost" | "/doctor" => "/status",
            "/settings" => "/config",
            other => other,
        };

        match command {
            "/q" | "/quit" | "/exit" => return Ok(false),
            "/stop" => {
                self.runtime.request_cancel();
                out.println("[stop] cancelling current turn…");
            }
            "/help" => {
                let mut help_lines = vec![
                    "nca Interactive Mode".into(),
                    String::new(),
                    "INPUT MODES:".into(),
                    "  ! <cmd>     Run shell command (output feeds into context)".into(),
                    "  @path       Inline file mentions".into(),
                    "  / <cmd>     Slash commands".into(),
                    "  \\           Multiline input (end line with \\ to continue)".into(),
                    String::new(),
                    "SLASH COMMANDS:".into(),
                ];
                help_lines.extend(registry_help_lines());
                help_lines.extend([
                    "KEYBOARD SHORTCUTS:".into(),
                    "  Tab          Cycle agent profile".into(),
                    "  Ctrl+P       Command palette".into(),
                    "  Ctrl+X M     Switch model".into(),
                    "  Ctrl+X E     Open editor".into(),
                    "  Ctrl+X L     Switch session".into(),
                    "  Ctrl+X N     New session".into(),
                    "  Ctrl+X C     Compact".into(),
                    "  Ctrl+X S     View status".into(),
                    "  Ctrl+X A     Agent picker".into(),
                    "  Ctrl+X H     Help".into(),
                    "  Ctrl+X Q     Exit".into(),
                    "  Ctrl+C       Cancel request".into(),
                    "  Ctrl+Shift+C Copy last assistant response".into(),
                    "  Ctrl+L       Clear screen".into(),
                    "  Ctrl+V       Paste image (TUI)".into(),
                    "  F2           Cycle recent models".into(),
                    "  Shift+drag   Native terminal selection fallback".into(),
                ]);
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("help", help_lines);
                    }
                } else {
                    for l in &help_lines {
                        out.println(l);
                    }
                }
            }
            "/status" => {
                let snapshot = self.runtime.snapshot();
                let mut lines = vec![
                    format!("Session:     {}", snapshot.id),
                    format!("Model:       {}", self.runtime.model()),
                    format!("Agent:       @{}", self.agent_profile.label()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Children:    {}", snapshot.child_session_ids.len()),
                    format!("Todos:       {}", snapshot.todos.len()),
                    format!("Memory:      {}", self.runtime.memory_store_path().display()),
                    format!(
                        "Reasoning:   {} (OpenAI-compatible only; {})",
                        self.runtime.config().model.reasoning_effort,
                        if self
                            .runtime
                            .config()
                            .reasoning_effort_active_for_default_provider()
                        {
                            "active"
                        } else {
                            "inactive for active provider"
                        }
                    ),
                    String::new(),
                    "Provider health:".into(),
                ];
                for provider in ProviderKind::ALL {
                    lines.push(format!(
                        "  {:<12} key {}",
                        provider.display_name(),
                        if self.runtime.config().provider.api_key_present_for(provider) {
                            "configured"
                        } else {
                            "missing"
                        }
                    ));
                }
                if let Some(summary) = &snapshot.session_summary {
                    lines.push(String::new());
                    lines.push(format!("Summary: {}", summary.replace('\n', " ")));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        if let Some(report) = &g.context_report {
                            lines.push(String::new());
                            lines.push("Context compaction:".into());
                            lines.push(format!("  phase     {}", report.phase));
                            lines.push(format!("  message   {}", report.message));
                            if let (Some(before), Some(after)) =
                                (report.tokens_before, report.tokens_after)
                            {
                                lines.push(format!("  tokens    {before} → {after}"));
                            }
                            if let (Some(retained), Some(dropped)) =
                                (report.retained_groups, report.dropped_groups)
                            {
                                lines.push(format!(
                                    "  groups    retained {retained}, dropped {dropped}"
                                ));
                            }
                        }
                        g.open_info_modal("status", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/agent" => {
                if let Some(target) = parts.next() {
                    let target_clean = target.trim_start_matches('@').to_lowercase();
                    let matched = AgentProfile::ALL.iter().find(|p| {
                        p.label() == target_clean
                    });
                    if let Some(profile) = matched {
                        self.agent_profile = *profile;
                        self.current_agent_label = format!("@{}", profile.label());
                        self.prompt.set_agent(&self.current_agent_label);
                        if *profile == AgentProfile::Plan {
                            self.runtime.set_permission_mode(PermissionMode::Plan);
                        } else {
                            self.runtime.set_permission_mode(PermissionMode::Default);
                        }
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.set_agent_profile(&self.current_agent_label);
                            g.set_permission_mode(&format!(
                                "{:?}",
                                self.runtime.permission_mode()
                            ));
                        }
                        out.println(&format!("Switched to @{} mode", profile.label()));
                    } else {
                        out.println(&format!("Unknown agent profile: {}", target));
                        out.println(&format!(
                            "Available: {}",
                            AgentProfile::ALL
                                .iter()
                                .map(|p| p.label())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else if let ReplOutput::Tui(st) = &out {
                    let current_idx = AgentProfile::ALL
                        .iter()
                        .position(|p| *p == self.agent_profile)
                        .unwrap_or(0);
                    if let Ok(mut g) = st.lock() {
                        g.open_agent_picker(current_idx);
                    }
                } else {
                    out.println(&format!("Current agent: @{}", self.agent_profile.label()));
                    out.println("Available profiles:");
                    for profile in AgentProfile::ALL {
                        let marker = if profile == self.agent_profile { " *" } else { "" };
                        out.println(&format!("  @{}{}", profile.label(), marker));
                    }
                }
            }
            "/plan" => {
                self.run_preset(
                    "Create a short implementation plan before coding. Focus on steps, risks, and validation.\n\nTask:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/review" => {
                self.run_preset(
                    "Review the requested code or changes. Prioritize bugs, regressions, risks, and missing tests.\n\nReview target:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/fix" => {
                self.run_preset(
                    "Diagnose and fix the issue below. Prefer a minimal verified change.\n\nIssue:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/test" => {
                self.run_preset(
                    "Validate the requested area. Run tests or checks if tools allow, and report what passed or failed.\n\nTarget:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/model" => {
                if let Some(model) = parts.next() {
                    let resolved = self.runtime.config().model.resolve_alias(model);
                    let mut cfg = self.runtime.config().clone();
                    cfg.apply_model_override(&resolved);
                    cfg.model.track_recent_model(&resolved);
                    match self.runtime.apply_nca_config(cfg) {
                        Ok(()) => {
                            if let Err(e) = self
                                .runtime
                                .config()
                                .save_workspace_file(self.runtime.workspace_root())
                            {
                                out.eprintln(&format!(
                                    "[model] session updated; workspace save failed: {e}"
                                ));
                            } else {
                                out.println(&format!(
                                    "model set to {} (saved .nca/config.local.toml)",
                                    self.runtime.model()
                                ));
                            }
                            if let ReplOutput::Tui(st) = out
                                && let Ok(mut g) = st.lock()
                            {
                                g.model = self.runtime.model().to_string();
                                g.mark_dirty();
                            }
                        }
                        Err(e) => out.eprintln(&format!("[model] {e}")),
                    }
                } else if let ReplOutput::Tui(st) = &out {
                    let provider_models = nca_runtime::model_limits_api::fetch_provider_model_ids(self.runtime.config()).await;
                    let entries = build_model_picker_entries(self.runtime.config(), &provider_models);
                    if let Ok(mut g) = st.lock() {
                        g.open_model_picker(entries);
                    }
                } else {
                    out.println(&format!("active model: {}", self.runtime.model()));
                    for p in ProviderKind::ALL {
                        out.println(&format!(
                            "  {} → {}",
                            p.display_name(),
                            self.runtime.config().provider.model_for(p)
                        ));
                    }
                    out.println("usage: /model <name>");
                }
            }
            "/clear" => {
                out.clear_screen();
            }
            "/copy" => {
                let text = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .ok()
                        .and_then(|g| g.last_assistant_text().map(str::to_string)),
                    ReplOutput::Stdio => None,
                };
                match text {
                    Some(content) => {
                        let result = tokio::task::spawn_blocking(move || {
                            crate::clipboard::set_clipboard_text(&content)
                        })
                        .await?;
                        match result {
                            Ok(()) => out.println("Copied last assistant response"),
                            Err(error) => out.eprintln(&format!(
                                "[copy] {error} — try Shift+drag terminal selection as a fallback"
                            )),
                        }
                    }
                    None => out.eprintln(
                        "[copy] No assistant response to copy yet — try Shift+drag terminal selection as a fallback",
                    ),
                }
            }
            "/todos" => {
                let lines = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| g.todo_modal_lines())
                        .unwrap_or_else(|_| {
                            vec!["Unable to read session todos (lock poisoned)".into()]
                        }),
                    ReplOutput::Stdio => {
                        let todos = self.runtime.todos();
                        let mut lines = Vec::new();
                        if todos.is_empty() {
                            lines.push("No todos yet.".into());
                        } else {
                            let done = todos
                                .iter()
                                .filter(|t| {
                                    matches!(
                                        t.status,
                                        nca_common::todo::TodoStatus::Completed
                                            | nca_common::todo::TodoStatus::Cancelled
                                    )
                                })
                                .count();
                            lines.push(format!("Session todos ({done}/{} done)", todos.len()));
                            for todo in &todos {
                                lines.push(format!(
                                    "{} [{}] {}",
                                    todo.status.glyph(),
                                    todo.id,
                                    todo.content
                                ));
                            }
                        }
                        lines
                    }
                };
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("todos", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/undo" => {
                out.eprintln("[undo] Not yet implemented - use /compact to save session state");
            }
            "/redo" => {
                out.eprintln("[redo] Not yet implemented");
            }
            "/diff" => {
                // Show recent file changes via git
                let output = Command::new("sh")
                    .arg("-c")
                    .arg("git diff --stat HEAD~5..HEAD 2>/dev/null || git diff --stat 2>/dev/null || echo 'No git changes'")
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match output {
                    Ok(cmd_out) => {
                        let diff = String::from_utf8_lossy(&cmd_out.stdout);
                        if diff.is_empty() {
                            out.println("[diff] No recent changes");
                        } else {
                            out.print(&diff);
                        }
                    }
                    Err(e) => out.eprintln(&format!("[diff] Failed: {e}")),
                }
            }
            "/cost" => {
                let snapshot = self.runtime.snapshot();
                out.eprintln(&format!("[cost] Session: {}", snapshot.id));
                out.eprintln("[cost] Use 'nca logs --follow' to see real-time token usage");
            }
            "/stats" => {
                let snapshot = self.runtime.snapshot();
                let lines = vec![
                    format!("Session:     {}", snapshot.id),
                    format!("Model:       {}", self.runtime.model()),
                    format!("Agent:       @{}", self.agent_profile.label()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Children:    {}", snapshot.child_session_ids.len()),
                    format!("Memory:      {}", self.runtime.memory_store_path().display()),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("stats", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/permissions" => {
                if let Some(mode) = parts.next() {
                    if let Some(parsed_mode) = parse_permission_mode(mode) {
                        self.runtime.set_permission_mode(parsed_mode);
                        if let ReplOutput::Tui(st) = out
                            && let Ok(mut g) = st.lock()
                        {
                            g.set_permission_mode(&format!("{parsed_mode:?}"));
                        }
                        out.println(&format!("permission mode set to {parsed_mode:?}"));
                    } else {
                        out.println(
                            "invalid mode; expected one of: default, plan, accept-edits, dont-ask, bypass-permissions",
                        );
                    }
                } else if let ReplOutput::Tui(st) = &out {
                    let current_idx = permission_mode_index(self.runtime.permission_mode());
                    if let Ok(mut g) = st.lock() {
                        g.open_permission_picker(current_idx);
                    }
                } else {
                    out.println(&format!(
                        "permission_mode: {:?}",
                        self.runtime.permission_mode()
                    ));
                }
            }
            "/permission-bypass" => {
                let sub = parts.next().unwrap_or("").trim();
                let target = match sub.to_ascii_lowercase().as_str() {
                    "" | "toggle" => {
                        if self.runtime.permission_mode() == PermissionMode::BypassPermissions {
                            PermissionMode::Default
                        } else {
                            PermissionMode::BypassPermissions
                        }
                    }
                    "on" | "enable" | "yes" | "1" => PermissionMode::BypassPermissions,
                    "off" | "disable" | "no" | "0" => PermissionMode::Default,
                    _ => {
                        out.println(
                            "usage: /permission-bypass [on|off|toggle] — default toggles bypass ↔ default",
                        );
                        return Ok(true);
                    }
                };
                self.runtime.set_permission_mode(target);
                if let ReplOutput::Tui(st) = out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_permission_mode(&format!("{target:?}"));
                }
                out.println(&format!("permission mode set to {target:?}"));
            }
            "/skills" => {
                let skills = SkillCatalog::discover(
                    self.runtime.workspace_root(),
                    &self.runtime.config().harness.skill_directories,
                )
                .map_err(anyhow::Error::msg)?;
                if skills.is_empty() {
                    let lines = vec!["No skills discovered.".into()];
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("skills", lines);
                        }
                    } else {
                        out.println("no skills discovered");
                    }
                } else {
                    let lines: Vec<String> = skills.iter().map(|s| s.summary_line()).collect();
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("skills", lines);
                        }
                    } else {
                        for l in &lines {
                            out.println(l);
                        }
                    }
                }
            }
            "/memory" => {
                if rest.is_empty() {
                    let store = MemoryStore::new(self.runtime.memory_store_path());
                    let mem = store.load().await.map_err(anyhow::Error::msg)?;
                    if mem.notes.is_empty() {
                        let lines = vec!["No memory notes stored.".into()];
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("memory", lines);
                            }
                        } else {
                            out.println("no memory notes stored");
                        }
                    } else {
                        let lines: Vec<String> = mem
                            .notes
                            .iter()
                            .rev()
                            .take(20)
                            .map(|note| {
                                format!("{} {} {}", note.id, note.kind, note.content.replace('\n', " "))
                            })
                            .collect();
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("memory", lines);
                            }
                        } else {
                            for l in lines.iter().take(5) {
                                out.println(l);
                            }
                        }
                    }
                } else {
                    self.runtime
                        .append_memory_note("note", Some(rest.to_string()))
                        .await
                        .map_err(anyhow::Error::msg)?;
                    out.println("memory note saved");
                }
            }
            "/compact" => {
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary.clone()))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                out.println(&format!("saved session summary:\n{}", summary));
            }
            "/models" => {
                if let ReplOutput::Tui(st) = &out {
                    let provider_models = nca_runtime::model_limits_api::fetch_provider_model_ids(self.runtime.config()).await;
                    let entries = build_model_picker_entries(self.runtime.config(), &provider_models);
                    if let Ok(mut g) = st.lock() {
                        g.open_model_picker(entries);
                    }
                } else {
                    let provider = self.runtime.config().provider.default;
                    out.println(&format!(
                        "default_provider={} default_model={} thinking={} budget={}",
                        provider.display_name(),
                        self.runtime.config().model.default_model,
                        self.runtime.config().model.enable_thinking,
                        self.runtime.config().model.thinking_budget
                    ));
                    out.println(&format!(
                        "reasoning_effort={} scope=OpenAI-compatible-only active={}",
                        self.runtime.config().model.reasoning_effort,
                        self.runtime
                            .config()
                            .reasoning_effort_active_for_default_provider()
                    ));
                    for provider in nca_common::config::ProviderKind::ALL {
                        out.println(&format!(
                            "  {} -> {} ({})",
                            provider.display_name(),
                            self.runtime.config().provider.model_for(provider),
                            self.runtime.config().provider.base_url_for(provider)
                        ));
                    }
                    for (alias, target) in &self.runtime.config().model.aliases {
                        out.println(&format!("  {alias} -> {target}"));
                    }
                }
            }
            "/mcp" => {
                let lines: Vec<String> = if self.runtime.config().mcp.servers.is_empty() {
                    vec!["No MCP servers configured.".into()]
                } else {
                    self.runtime
                        .config()
                        .mcp
                        .servers
                        .iter()
                        .filter(|server| server.enabled)
                        .map(|server| {
                            format!(
                                "{} command={} {}",
                                server.name,
                                server.command,
                                server.args.join(" ")
                            )
                        })
                        .collect()
                };
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("mcp", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/agents" => {
                let snapshot = self.runtime.snapshot();
                let lines: Vec<String> = if snapshot.child_session_ids.is_empty() {
                    vec!["No child sessions yet.".into()]
                } else {
                    snapshot.child_session_ids.clone()
                };
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("agents", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/logs" => {
                match tokio::fs::read_to_string(self.runtime.event_log_path()).await {
                    Ok(data) => {
                        if let ReplOutput::Tui(st) = &out {
                            let lines: Vec<String> = data.lines().rev().take(100).map(String::from).collect();
                            let lines: Vec<String> = lines.into_iter().rev().collect();
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("logs (last 100)", lines);
                            }
                        } else {
                            out.print(&data);
                        }
                    }
                    Err(err) => {
                        out.eprintln(&format!("failed to read log: {err}"))
                    }
                }
            }
            "/attach" => {
                let snapshot = self.runtime.snapshot();
                let lines = vec![
                    format!("Session:  {}", snapshot.id),
                    format!(
                        "Socket:   {}",
                        snapshot
                            .socket_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "<none>".into())
                    ),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("attach", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/image" => {
                let st = match &out {
                    ReplOutput::Tui(st) => st,
                    ReplOutput::Stdio => {
                        out.eprintln(
                            "[image] stage images from the full-screen TUI (Ctrl+V, /image paste, /image <path>)",
                        );
                        return Ok(true);
                    }
                };
                let workspace = self.runtime.workspace_root().to_path_buf();
                let sid = self.runtime.session_id().to_string();
                let rest_trim = rest.trim();
                if rest_trim.is_empty() || rest_trim.eq_ignore_ascii_case("paste") {
                    out.println("[image] reading clipboard…");
                    let result = tokio::task::spawn_blocking(move || {
                        crate::image_attach::paste_clipboard_image(&workspace, &sid)
                    })
                    .await?;
                    match result {
                        Ok(att) => {
                            let path = att.path.clone();
                            let n = if let Ok(mut g) = st.lock() {
                                g.staged_image_attachments.push(att);
                                g.mark_dirty();
                                g.staged_image_attachments.len()
                            } else {
                                0
                            };
                            out.println(&format!(
                                "[image] staged {path} — press Enter to send ({n} attached)"
                            ));
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                } else if rest_trim.eq_ignore_ascii_case("clear") {
                    if let Ok(mut g) = st.lock() {
                        g.staged_image_attachments.clear();
                        g.mark_dirty();
                    }
                    out.println("[image] cleared staged images");
                } else {
                    let p = std::path::Path::new(rest_trim);
                    match crate::image_attach::import_image_file(&workspace, &sid, p) {
                        Ok(att) => {
                            let path = att.path.clone();
                            let n = if let Ok(mut g) = st.lock() {
                                g.staged_image_attachments.push(att);
                                g.mark_dirty();
                                g.staged_image_attachments.len()
                            } else {
                                0
                            };
                            out.println(&format!(
                                "[image] staged {path} — press Enter to send ({n} attached)"
                            ));
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                }
            }
            "/config" => {
                let config = self.runtime.config();
                let mut lines = vec![
                    format!("Provider:    {}", config.provider.default.display_name()),
                    format!("Model:       {}", self.runtime.model()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Memory:      {}", self.runtime.memory_store_path().display()),
                    format!("Editor:      {}", config.effective_editor_command()),
                    format!(
                        "Reasoning:   {} (OpenAI-compatible only; {})",
                        config.model.reasoning_effort,
                        if config.reasoning_effort_active_for_default_provider() {
                            "active"
                        } else {
                            "inactive for active provider"
                        }
                    ),
                    format!("Thinking:    {} (budget: {})", config.model.enable_thinking, config.model.thinking_budget),
                    format!("Max tokens:  {}", config.model.max_tokens),
                    String::new(),
                    "Provider endpoints:".into(),
                ];
                for provider in ProviderKind::ALL {
                    lines.push(format!(
                        "  {:<12} {}",
                        format!("{}:", provider.display_name()),
                        config.provider.base_url_for(provider)
                    ));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("config", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/connect" => {
                if let ReplOutput::Tui(st) = out {
                    if let Ok(mut g) = st.lock() {
                        g.open_connect_modal();
                    }
                    out.println(
                        "[connect] Choose a provider (↑↓ · Enter · type to search · Esc). Add key with /apikey if needed.",
                    );
                } else {
                    out.println("Connect an LLM provider (non-TUI):");
                    out.println("  /provider <minimax|openai|anthropic|openrouter|custom>");
                    out.println("  /custom <openai|anthropic> <base-url> [api-key] [model]");
                    out.println("  /apikey <provider> <secret>   — save API key to .nca/config.local.toml");
                    out.println("  /model <name>                 — set model after switching provider");
                    out.println(&format!(
                        "  current: {} → {}",
                        self.runtime.config().provider.default.display_name(),
                        self.runtime.model()
                    ));
                }
            }
            "/settings" => {
                let lines = vec![
                    "Workspace settings (.nca/config.local.toml):".into(),
                    String::new(),
                    format!("  Provider:    {}", self.runtime.config().provider.default.display_name()),
                    format!("  Model:       {}", self.runtime.model()),
                    format!("  Editor:      {}", self.runtime.config().effective_editor_command()),
                    format!("  Permission:  {:?}", self.runtime.permission_mode()),
                    String::new(),
                    "Commands:".into(),
                    "  /connect           OpenCode-style provider picker".into(),
                    "  /models            Browse and select models".into(),
                    "  /provider [name]   Default LLM provider".into(),
                    "  /custom            Configure custom endpoint".into(),
                    "  /apikey <p> <key>  Store API key for a provider".into(),
                    "  /model [name]      Model for the active provider".into(),
                    "  /editor [seed]     Open external editor".into(),
                    "  /set-editor <cmd>  Persist editor command".into(),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("settings", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/provider" => {
                let rest = rest.trim();
                if rest.is_empty() {
                    if let ReplOutput::Tui(st) = out {
                        if let Ok(mut g) = st.lock() {
                            g.open_provider_picker(self.runtime.config().provider.default, false);
                        }
                        out.println("[provider] choose with ↑↓ + Enter, Esc cancel, c = custom API help");
                    } else {
                        out.println(&format!(
                            "current default provider: {} (model {})",
                            self.runtime.config().provider.default.display_name(),
                            self.runtime.model()
                        ));
                        out.println("usage: /provider <minimax|openai|anthropic|openrouter|custom>");
                        out.println("       /provider add-custom   (TUI: BYO endpoint wizard)");
                    }
                } else if rest.eq_ignore_ascii_case("add-custom")
                    || rest.eq_ignore_ascii_case("custom-wizard")
                {
                    if let ReplOutput::Tui(st) = out {
                        if let Ok(mut g) = st.lock() {
                            g.open_custom_provider_setup_from_config(
                                &self.runtime.config().provider.custom,
                            );
                        }
                        out.println("[provider] add custom provider wizard opened");
                    } else {
                        out.println("Wizard needs the full-screen TUI. Use:");
                        out.println("  /custom <openai|anthropic> <base-url> [api-key] [model]");
                    }
                } else if let Some(p) = ProviderKind::from_cli_name(rest)
                    .or_else(|| ProviderKind::parse_display_name(rest))
                {
                    self.apply_provider_in_session(p, out).await?;
                } else {
                    out.eprintln("unknown provider; try: minimax, openai, anthropic, openrouter, custom, add-custom");
                }
            }
            "/custom" => {
                let command = match parse_custom_provider_command(rest) {
                    Ok(Some(command)) => command,
                    Ok(None) => {
                        let custom = &self.runtime.config().provider.custom;
                        let lines = vec![
                            format!(
                                "Compatibility: {}",
                                custom.compatibility.display_name()
                            ),
                            format!(
                                "Base URL:      {}",
                                if custom.base_url.trim().is_empty() {
                                    "<not set>"
                                } else {
                                    custom.base_url.as_str()
                                }
                            ),
                            format!("Model:         {}", custom.model),
                            format!("API key env:   {}", custom.api_key_env),
                            format!(
                                "API key:       {}",
                                if custom.resolve_api_key().is_some() {
                                    "configured"
                                } else {
                                    "missing"
                                }
                            ),
                            String::new(),
                            "usage: /custom <openai|anthropic> <base-url> [api-key] [model]".into(),
                            "TUI:   /provider → \"Add custom provider…\"".into(),
                            "example: /custom openai https://sumopod.example sk-test my-model".into(),
                        ];
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("custom provider", lines);
                            }
                        } else {
                            for line in &lines {
                                out.println(line);
                            }
                        }
                        return Ok(true);
                    }
                    Err(error) => {
                        out.eprintln(error);
                        return Ok(true);
                    }
                };
                let CustomProviderCommand {
                    compatibility,
                    base_url,
                    api_key,
                    model,
                } = command;
                let api_key_env = self.runtime.config().provider.custom.api_key_env.clone();
                self.persist_custom_provider_config(
                    compatibility,
                    base_url,
                    api_key_env,
                    api_key,
                    model,
                    out,
                )
                .await?;
            }
            "/apikey" => {
                let mut toks = rest.split_whitespace();
                let p_name = toks.next();
                let key = toks.collect::<Vec<_>>().join(" ");
                let key = key.trim();
                if let Some(pn) = p_name {
                    let p = ProviderKind::from_cli_name(pn)
                        .or_else(|| ProviderKind::parse_display_name(pn));
                    if let Some(p) = p {
                        if key.is_empty() {
                            if let ReplOutput::Tui(st) = out {
                                if let Ok(mut g) = st.lock() {
                                    g.open_api_key_modal(
                                        p,
                                        self.runtime.config().provider.api_key_present_for(p),
                                        false,
                                    );
                                }
                            } else {
                                out.println("usage: /apikey <provider> <secret>");
                            }
                        } else {
                            self.save_provider_api_key(p, key, out).await?;
                        }
                    } else {
                        out.eprintln("unknown provider; try: minimax, openai, anthropic, openrouter, custom");
                    }
                } else if let ReplOutput::Tui(st) = out {
                    if let Ok(mut g) = st.lock() {
                        g.open_provider_picker(self.runtime.config().provider.default, true);
                    }
                    out.println("[apikey] pick provider, then paste key + Enter");
                } else {
                    out.println("usage: /apikey <provider> <secret>");
                }
            }
            "/editor" => {
                let seed = if rest.is_empty() { None } else { Some(rest) };
                match self.open_external_editor(seed).await {
                    Some(text) if !text.is_empty() => {
                        if let ReplOutput::Tui(st) = out {
                            if let Ok(mut g) = st.lock() {
                                g.input_buffer = text;
                                g.cursor_char_idx = g.input_buffer.chars().count();
                                g.mark_dirty();
                            }
                            out.println("[editor] loaded into composer — press Enter to send");
                        } else {
                            let expanded = match expand_at_file_mentions_default(
                                &text,
                                self.runtime.workspace_root(),
                            ) {
                                Ok(s) => s,
                                Err(e) => {
                                    out.eprintln(&format!("file mention expansion: {e}"));
                                    text
                                }
                            };
                            match self.runtime.run_turn(&expanded).await {
                                Ok(o) => println!("{o}"),
                                Err(e) => eprintln!("error: {e}"),
                            }
                        }
                    }
                    Some(_) => out.println("[editor] empty buffer — nothing sent"),
                    None => {}
                }
            }
            "/set-editor" => {
                let cmd = rest.trim();
                if cmd.is_empty() {
                    out.println(&format!(
                        "usage: /set-editor <command>  (effective: {})",
                        self.runtime.config().effective_editor_command()
                    ));
                } else {
                    self.runtime.config_mut().ui.editor = Some(cmd.to_string());
                    match self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
                        Ok(()) => out.println(&format!(
                            "[set-editor] saved `{cmd}` to .nca/config.local.toml"
                        )),
                        Err(e) => out.eprintln(&format!("[set-editor] save failed: {e}")),
                    }
                }
            }
            "/doctor" => {
                let mut lines = Vec::new();
                for provider in nca_common::config::ProviderKind::ALL {
                    let configured = self
                        .runtime
                        .config()
                        .provider
                        .api_key_present_for(provider);
                    lines.push(format!(
                        "{}{} API key {} ({})",
                        provider.display_name(),
                        if provider == self.runtime.config().provider.default {
                            " [selected]"
                        } else {
                            ""
                        },
                        if configured { "✓ configured" } else { "✗ missing" },
                        self.runtime.config().provider.api_key_env_for(provider)
                    ));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("doctor", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/auto-answer" => {
                let from_tui = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .and_then(|g| g.active_question.as_ref().map(|q| q.question_id.clone()))
                } else {
                    None
                };
                let ok = if let Some(qid) = from_tui {
                    self.runtime
                        .submit_question_answer(&qid, QuestionSelection::Suggested)
                } else {
                    self.runtime.submit_suggested_answer()
                };
                if ok {
                    out.println("accepted suggested answer for pending question");
                } else {
                    out.eprintln(
                        "no pending interactive question to auto-answer (use when ask_question is waiting)",
                    );
                }
            }
            "/sessions" => match self.runtime.list_session_ids().await {
                Ok(mut ids) => {
                    ids.sort();
                    if ids.is_empty() {
                        let lines = vec!["No saved sessions.".into()];
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("sessions", lines);
                            }
                        } else {
                            out.println("no saved sessions");
                        }
                    } else if let ReplOutput::Tui(st) = &out {
                        let current = self.runtime.session_id().to_string();
                        if let Ok(mut g) = st.lock() {
                            g.open_session_picker(ids, &current);
                        }
                    } else {
                        for id in ids {
                            out.println(&id);
                        }
                    }
                }
                Err(error) => {
                    out.eprintln(&format!("failed to list sessions: {error}"));
                }
            },
            "/new" => {
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                self.runtime.new_session().await.map_err(anyhow::Error::msg)?;
                let new_id = self.runtime.session_id().to_string();
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.blocks.clear();
                    g.streaming_assistant = None;
                    g.scroll_lines = 0;
                    g.transcript_follow_tail = true;
                    g.session_id = new_id.clone();
                    g.model = self.runtime.model().to_string();
                    g.input_tokens = 0;
                    g.output_tokens = 0;
                    g.cost_usd = 0.0;
                    g.started = std::time::Instant::now();
                    g.mark_transcript_dirty();
                }
                out.println(&format!("new session started: {new_id}"));
            }
            "/export" => {
                let snapshot = self.runtime.snapshot();
                let events = self.runtime.event_log_path();
                let md = match tokio::fs::read_to_string(&events).await {
                    Ok(raw) => {
                        let mut md_lines = vec![
                            format!("# Session {}", snapshot.id),
                            String::new(),
                        ];
                        for line in raw.lines() {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                                && let Some(kind) = val.get("kind").and_then(|v| v.as_str())
                            {
                                match kind {
                                    "MessageReceived" => {
                                        let role = val.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                                        let content =
                                            val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                        md_lines.push(format!("## {role}"));
                                        md_lines.push(String::new());
                                        md_lines.push(content.to_string());
                                        md_lines.push(String::new());
                                    }
                                    "ToolCallStarted" => {
                                        let tool = val.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                                        md_lines.push(format!("### tool: {tool}"));
                                        md_lines.push(String::new());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        md_lines.join("\n")
                    }
                    Err(e) => {
                        out.eprintln(&format!("[export] failed to read event log: {e}"));
                        return Ok(true);
                    }
                };
                let export_path = self.runtime.workspace_root().join(format!(".nca/export-{}.md", snapshot.id));
                if let Some(parent) = export_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match tokio::fs::write(&export_path, &md).await {
                    Ok(()) => out.println(&format!("exported to {}", export_path.display())),
                    Err(e) => out.eprintln(&format!("[export] {e}")),
                }
            }
            "/thinking" => {
                let mut cfg = self.runtime.config().clone();
                cfg.model.enable_thinking = !cfg.model.enable_thinking;
                let new_state = cfg.model.enable_thinking;
                match self.runtime.apply_nca_config(cfg) {
                    Ok(()) => {
                        if let Err(e) = self.runtime.config().save_workspace_file(self.runtime.workspace_root()) {
                            out.eprintln(&format!("[thinking] toggled but save failed: {e}"));
                        } else {
                            out.println(&format!("thinking {} (budget: {})", if new_state { "enabled" } else { "disabled" }, self.runtime.config().model.thinking_budget));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[thinking] {e}")),
                }
            }
            "/reasoning-effort" => {
                let value = rest.trim();
                if value.is_empty() {
                    let config = self.runtime.config();
                    out.println(&format!(
                        "reasoning effort: {} (OpenAI-compatible only; {})",
                        config.model.reasoning_effort,
                        if config.reasoning_effort_active_for_default_provider() {
                            "active"
                        } else {
                            "inactive for active provider"
                        }
                    ));
                } else {
                    let mut cfg = self.runtime.config().clone();
                    cfg.model.reasoning_effort = value.to_string();
                    match self.runtime.apply_nca_config(cfg) {
                        Ok(()) => {
                            if let Err(error) = self
                                .runtime
                                .config()
                                .save_workspace_file(self.runtime.workspace_root())
                            {
                                out.eprintln(&format!(
                                    "[reasoning-effort] applied but workspace save failed: {error}"
                                ));
                            } else {
                                out.println(&format!(
                                    "reasoning effort set to {} (OpenAI-compatible only)",
                                    self.runtime.config().model.reasoning_effort
                                ));
                            }
                        }
                        Err(error) => out.eprintln(&format!("[reasoning-effort] {error}")),
                    }
                }
            }
            _ => {
                if command.starts_with('/')
                    && self
                        .try_run_skill(command.trim_start_matches('/'), rest, &out)
                        .await?
                {
                    return Ok(true);
                }
                out.eprintln(&format!("unknown command: {command}"));
            }
        }

        Ok(true)
    }

    async fn run_preset(
        &mut self,
        prefix: &str,
        task: &str,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        if task.trim().is_empty() {
            out.println("usage: /<command> <task description>");
            return Ok(());
        }
        let prompt = format!("{prefix}{}", task.trim());
        let prompt = match expand_at_file_mentions_default(&prompt, self.runtime.workspace_root()) {
            Ok(s) => s,
            Err(e) => {
                out.eprintln(&format!("file mentions: {e}"));
                return Ok(());
            }
        };
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(())
    }

    async fn try_run_skill(
        &mut self,
        skill_name: &str,
        task: &str,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        let skills = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        )
        .map_err(anyhow::Error::msg)?;
        let Some(skill) = skills.into_iter().find(|skill| skill.command == skill_name) else {
            return Ok(false);
        };

        if let Some(model) = &skill.model {
            self.runtime
                .set_model(self.runtime.config().model.resolve_alias(model));
        }
        if let Some(mode) = skill.permission_mode {
            self.runtime.set_permission_mode(mode);
        }

        let prompt = skill.prompt_for_task(task);
        let prompt = match expand_at_file_mentions_default(&prompt, self.runtime.workspace_root()) {
            Ok(s) => s,
            Err(e) => {
                out.eprintln(&format!("file mentions: {e}"));
                return Ok(true);
            }
        };
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(true)
    }

    /// Full-screen TUI: transcript + streaming + composer (default on TTY).
    pub async fn run_with_tui(&mut self) -> anyhow::Result<()> {
        let session_id = self.runtime.session_id().to_string();
        let model = self.runtime.model().to_string();
        let perm = if self.runtime.execution_context().yolo {
            "YOLO".to_string()
        } else {
            format!("{:?}", self.runtime.permission_mode())
        };
        let shared_state = SharedTuiState::new(TuiSessionState::new(
            session_id,
            model,
            self.current_agent_label.clone(),
            perm,
            self.runtime.workspace_root().to_path_buf(),
        ));
        let tui_state = shared_state.arc();
        if let Ok(mut g) = tui_state.lock() {
            let provider = self.runtime.config().provider.default;
            g.set_active_provider(
                provider,
                self.runtime.config().provider.base_url_for(provider),
            );
        }

        // Seed from the authoritative session snapshot before replaying events so
        // resume still shows todos even if the event log is truncated.
        let snapshot_todos = self.runtime.todos();
        if !snapshot_todos.is_empty()
            && let Ok(mut g) = tui_state.lock()
        {
            g.set_todos(snapshot_todos);
        }

        let log_path = self.runtime.event_log_path();
        replay_event_log_into_state(&log_path, &tui_state).await;

        // Populate the git branch name immediately so it appears on first render.
        let workspace = self.runtime.workspace_root().to_path_buf();
        let initial_branch =
            tokio::task::spawn_blocking(move || git_current_branch(&workspace)).await?;
        if let Some(branch) = initial_branch
            && let Ok(mut g) = tui_state.lock()
        {
            g.set_current_branch(&branch);
        }

        let rx = self
            .runtime
            .take_event_rx()
            .ok_or_else(|| anyhow::anyhow!("internal: event channel already taken"))?;
        let ipc = self.runtime.take_ipc_handle();
        let approval = self.runtime.take_ipc_approval_pending();
        let question = self.runtime.question_pending();
        let _bridge = spawn_tui_bridge(
            rx,
            log_path,
            ipc,
            approval.clone(),
            question.clone(),
            tui_state.clone(),
            Some(shared_state.version_tx()),
        );

        let _spawn_task = {
            let spawn_rx = self.runtime.take_spawn_rx();
            let event_tx = self.runtime.event_tx();
            if let Some(srx) = spawn_rx {
                Some(nca_runtime::supervisor::spawn_subagent_consumer(
                    srx,
                    self.runtime.session_id().to_string(),
                    self.runtime.workspace_root().to_path_buf(),
                    self.runtime.config().clone(),
                    self.runtime.execution_context(),
                    self.runtime.safe_mode(),
                    self.runtime.messages().to_vec(),
                    event_tx,
                ))
            } else {
                None
            }
        };

        // Answers must bypass the main `cmd_rx` loop: while `run_turn` is blocked inside
        // `ask_question`, that task never receives `TuiCmd::Submit` or `QuestionAnswer`.
        let (answer_tx, mut answer_rx) =
            tokio::sync::mpsc::channel::<(String, QuestionSelection)>(16);
        let qp_dispatch = question.clone();
        tokio::spawn(async move {
            while let Some((qid, sel)) = answer_rx.recv().await {
                let _ = dispatch_question_answer(&qp_dispatch, &qid, sel);
            }
        });
        let answer_for_tui = answer_tx.clone();
        drop(answer_tx);

        let (approval_tx, mut approval_rx) = tokio::sync::mpsc::channel::<ApprovalAnswer>(16);
        let approval_dispatch = approval.clone();
        let approval_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some(answer) = approval_rx.recv().await {
                let (call_id, verdict) = match answer {
                    ApprovalAnswer::Verdict { call_id, approved } => (
                        call_id,
                        if approved {
                            nca_core::approval::ApprovalVerdict::Approved
                        } else {
                            nca_core::approval::ApprovalVerdict::Denied
                        },
                    ),
                    ApprovalAnswer::AllowPattern { call_id, pattern } => (
                        call_id,
                        nca_core::approval::ApprovalVerdict::AllowPattern(pattern),
                    ),
                };
                if !dispatch_tool_approval(&approval_dispatch, &call_id, verdict)
                    && let Ok(mut g) = approval_state.lock()
                {
                    g.clear_active_approval_if_matches(&call_id);
                    g.push_error(
                        "approval was no longer pending; cleared stale approval state".into(),
                    );
                }
            }
        });
        let approval_for_tui = approval_tx.clone();
        drop(approval_tx);

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<TuiCmd>(64);
        let st = tui_state.clone();
        let version_rx = shared_state.version_rx();
        let version_tx = shared_state.version_tx();
        let banner = self.run_mode;
        let cancel_flag = self.runtime.cancel_handle();
        let ui = tokio::task::spawn_blocking(move || {
            run_blocking(
                st,
                version_rx,
                version_tx,
                cmd_tx,
                Some(answer_for_tui),
                Some(approval_for_tui),
                banner,
                Some(cancel_flag),
            )
        });

        loop {
            let cmd = cmd_rx.recv().await;
            let Some(cmd) = cmd else { break };
            match cmd {
                TuiCmd::Exit => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.should_exit = true;
                        g.mark_dirty();
                    }
                    break;
                }
                TuiCmd::CycleAgent => {
                    let next = self.agent_profile.next();
                    self.agent_profile = next;
                    self.current_agent_label = format!("@{}", next.label());
                    if next == AgentProfile::Plan {
                        self.runtime.set_permission_mode(PermissionMode::Plan);
                    } else {
                        self.runtime.set_permission_mode(PermissionMode::Default);
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_agent_profile(&self.current_agent_label);
                        g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                    }
                }
                TuiCmd::CancelTurn => {
                    self.runtime.request_cancel();
                    // Unblock ask_question / approval waiters so the turn can exit.
                    if let Some(qp) = self.runtime.question_pending()
                        && let Ok(mut m) = qp.lock()
                    {
                        m.clear();
                    }
                    if let Some(ref ap) = approval
                        && let Ok(mut m) = ap.lock()
                    {
                        for (_, tx) in m.drain() {
                            let _ = tx.send(nca_core::approval::ApprovalVerdict::Denied);
                        }
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.dismiss_interactive_prompts();
                    }
                }
                TuiCmd::OpenBranchPicker => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_branch_picker(Vec::new(), "");
                    }
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    let (branches, current) = tokio::task::spawn_blocking(move || {
                        let branches = git_list_branches(&workspace);
                        let current = git_current_branch(&workspace).unwrap_or_default();
                        (branches, current)
                    })
                    .await?;
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_branch_picker(branches, &current);
                        g.set_current_branch(&current);
                    }
                }
                TuiCmd::SwitchBranch(name) => {
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    let branch = name.clone();
                    let switched =
                        tokio::task::spawn_blocking(move || git_switch_branch(&workspace, &branch))
                            .await?;
                    if switched {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_current_branch(&name);
                            g.blocks.push(DisplayBlock::System(format!(
                                "Switched to branch: {}",
                                name
                            )));
                            g.mark_transcript_dirty();
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.push_error(format!("Failed to switch to branch: {}", name));
                    }
                }
                TuiCmd::CreateBranch(name) => {
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    let branch = name.clone();
                    let created =
                        tokio::task::spawn_blocking(move || git_create_branch(&workspace, &branch))
                            .await?;
                    if created {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_current_branch(&name);
                            g.blocks.push(DisplayBlock::System(format!(
                                "Created and switched to branch: {}",
                                name
                            )));
                            g.mark_transcript_dirty();
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.push_error(format!("Failed to create branch: {}", name));
                    }
                }
                TuiCmd::PasteClipboard => {
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    let session_id = self.runtime.session_id().to_string();
                    let result = tokio::task::spawn_blocking(move || {
                        crate::image_attach::paste_clipboard_image(&workspace, &session_id)
                    })
                    .await?;
                    match result {
                        Ok(attachment) => {
                            let label = attachment.path.clone();
                            if let Ok(mut g) = tui_state.lock() {
                                g.staged_image_attachments.push(attachment);
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[image] staged {label} — Enter to send"
                                )));
                                g.mark_transcript_dirty();
                            }
                        }
                        Err(error) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[image] {error}"));
                            }
                        }
                    }
                }
                TuiCmd::CopyLastAssistant => {
                    self.handle_command("/copy", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::ApplyDefaultProvider(p) => {
                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::ConfigureCustomProvider => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_custom_provider_setup_from_config(
                            &self.runtime.config().provider.custom,
                        );
                        g.blocks.push(DisplayBlock::System(
                            "[connect] custom provider setup opened".into(),
                        ));
                        g.mark_transcript_dirty();
                    }
                }
                TuiCmd::ApplyCustomProviderSetup {
                    compatibility,
                    base_url,
                    api_key_env,
                    api_key,
                    model,
                } => {
                    self.finish_custom_setup_submission(
                        CustomProviderSetupSubmission {
                            compatibility,
                            base_url,
                            api_key_env,
                            api_key,
                            model,
                        },
                        &tui_state,
                        true,
                    )
                    .await?;
                }
                TuiCmd::CustomProviderProbeAction(action) => match action {
                    CustomProviderProbeAction::Cancel => {
                        self.pending_custom_setup = None;
                        if let Ok(mut g) = tui_state.lock() {
                            g.close_custom_provider_setup();
                        }
                    }
                    CustomProviderProbeAction::Retry => {
                        if let Some(submission) = self.pending_custom_setup.clone() {
                            self.finish_custom_setup_submission(submission, &tui_state, true)
                                .await?;
                        }
                    }
                    CustomProviderProbeAction::SaveAnyway => {
                        if let Some(submission) = self.pending_custom_setup.clone() {
                            self.finish_custom_setup_submission(submission, &tui_state, false)
                                .await?;
                        }
                    }
                },
                TuiCmd::PromptApiKey(p, connect_after_save) => {
                    if let Ok(mut g) = tui_state.lock() {
                        if p == ProviderKind::Custom && self.custom_setup_required() {
                            g.open_custom_provider_setup_from_config(
                                &self.runtime.config().provider.custom,
                            );
                            g.blocks.push(DisplayBlock::System(
                                "[apikey] configure custom endpoint first (wizard opened)".into(),
                            ));
                            g.mark_transcript_dirty();
                        } else {
                            g.open_api_key_modal(
                                p,
                                self.runtime.config().provider.api_key_present_for(p),
                                connect_after_save,
                            );
                        }
                    }
                }
                TuiCmd::ApplyModel(model_name) => {
                    let resolved = self.runtime.config().model.resolve_alias(&model_name);
                    let mut cfg = self.runtime.config().clone();
                    cfg.apply_model_override(&resolved);
                    cfg.model.track_recent_model(&resolved);
                    match self.runtime.apply_nca_config(cfg) {
                        Ok(()) => {
                            if let Err(e) = self
                                .runtime
                                .config()
                                .save_workspace_file(self.runtime.workspace_root())
                            {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("[model] workspace save failed: {e}"));
                                }
                            } else if let Ok(mut g) = tui_state.lock() {
                                g.model = self.runtime.model().to_string();
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[model] switched to {} (saved)",
                                    self.runtime.model()
                                )));
                                g.mark_transcript_dirty();
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[model] {e}"));
                            }
                        }
                    }
                }
                TuiCmd::ApplyModelProvider(p) => {
                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::ApplyPermission(idx) => {
                    let mode = permission_mode_from_index(idx);
                    self.runtime.set_permission_mode(mode);
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_permission_mode(&format!("{mode:?}"));
                        g.blocks.push(DisplayBlock::System(format!(
                            "permission mode set to {mode:?}"
                        )));
                        g.mark_transcript_dirty();
                    }
                }
                TuiCmd::SwitchAgent(idx) => {
                    if let Some(&profile) = AgentProfile::ALL.get(idx) {
                        self.agent_profile = profile;
                        self.current_agent_label = format!("@{}", profile.label());
                        if profile == AgentProfile::Plan {
                            self.runtime.set_permission_mode(PermissionMode::Plan);
                        } else {
                            self.runtime.set_permission_mode(PermissionMode::Default);
                        }
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_agent_profile(&self.current_agent_label);
                            g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                            g.blocks.push(DisplayBlock::System(format!(
                                "switched to @{}",
                                profile.label()
                            )));
                            g.mark_transcript_dirty();
                        }
                    }
                }
                TuiCmd::OpenEditor => {
                    self.handle_command("/editor", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::NewSession => {
                    self.handle_command("/new", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::RunCompact => {
                    self.handle_command("/compact", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenModelPicker => {
                    self.handle_command("/models", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenStatus => {
                    self.handle_command("/status", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenHelp => {
                    self.handle_command("/help", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenAgentPicker => {
                    let current_idx = AgentProfile::ALL
                        .iter()
                        .position(|p| *p == self.agent_profile)
                        .unwrap_or(0);
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_agent_picker(current_idx);
                    }
                }
                TuiCmd::OpenPermissionPicker => {
                    let current_idx = permission_mode_index(self.runtime.permission_mode());
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_permission_picker(current_idx);
                    }
                }
                TuiCmd::OpenSessions => {
                    self.handle_command("/sessions", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::ResumeSession(session_id) => {
                    let current = self.runtime.session_id().to_string();
                    if session_id == current {
                        if let Ok(mut g) = tui_state.lock() {
                            g.blocks
                                .push(DisplayBlock::System("Already on this session.".into()));
                            g.mark_transcript_dirty();
                        }
                    } else {
                        // Save current, then attempt resume
                        let _ = self.runtime.save().await;
                        if let Ok(mut g) = tui_state.lock() {
                            g.blocks.push(DisplayBlock::System(format!(
                                "Resuming session {} is not yet fully supported in-process. Please restart nca with: nca resume {session_id}",
                                session_id
                            )));
                            g.mark_transcript_dirty();
                        }
                    }
                }
                TuiCmd::CycleModel(forward) => {
                    let recent = &self.runtime.config().model.recent_models;
                    if recent.len() >= 2 {
                        let current = self.runtime.model().to_string();
                        let pos = recent.iter().position(|m| m == &current).unwrap_or(0);
                        let next_pos = if forward {
                            (pos + 1) % recent.len()
                        } else {
                            pos.checked_sub(1).unwrap_or(recent.len() - 1)
                        };
                        let next_model = recent[next_pos].clone();
                        let mut cfg = self.runtime.config().clone();
                        cfg.apply_model_override(&next_model);
                        if let Ok(()) = self.runtime.apply_nca_config(cfg) {
                            let _ = self
                                .runtime
                                .config()
                                .save_workspace_file(self.runtime.workspace_root());
                            if let Ok(mut g) = tui_state.lock() {
                                g.model = self.runtime.model().to_string();
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[F2] switched to {}",
                                    self.runtime.model()
                                )));
                                g.mark_transcript_dirty();
                            }
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.blocks.push(DisplayBlock::System(
                            "[F2] no recent models to cycle (need 2+ in model.recent_models)"
                                .into(),
                        ));
                        g.mark_transcript_dirty();
                    }
                }
                TuiCmd::ValidateApiKey(provider, api_key) => {
                    // Set validating state
                    if let Ok(mut g) = tui_state.lock() {
                        g.validation_status =
                            Some(crate::tui::state::OnboardingValidation::Validating);
                        g.mark_dirty();
                    }
                    // Look up base_url from config
                    let base_url = self
                        .runtime
                        .config()
                        .provider
                        .base_url_for(provider)
                        .to_string();
                    // Run async validation
                    let result = nca_core::provider::validate::validate_api_key(
                        provider,
                        &api_key,
                        &base_url,
                        (provider == ProviderKind::Custom)
                            .then_some(self.runtime.config().provider.custom.compatibility),
                    )
                    .await;
                    if let Ok(mut g) = tui_state.lock() {
                        match &result {
                            nca_core::provider::validate::ValidationResult::Valid => {
                                // Save key and complete onboarding
                                g.validation_status =
                                    Some(crate::tui::state::OnboardingValidation::Valid);
                                g.close_api_key_modal();
                                g.close_connect_modal();
                                g.onboarding_mode = false;
                            }
                            nca_core::provider::validate::ValidationResult::InvalidKey(msg) => {
                                g.validation_status = Some(
                                    crate::tui::state::OnboardingValidation::Failed(msg.clone()),
                                );
                                g.mark_dirty();
                            }
                            nca_core::provider::validate::ValidationResult::NetworkError(msg) => {
                                g.validation_status = Some(
                                    crate::tui::state::OnboardingValidation::Failed(msg.clone()),
                                );
                                g.mark_dirty();
                            }
                        }
                    }
                    // If validation succeeded, save key + complete onboarding
                    if matches!(
                        result,
                        nca_core::provider::validate::ValidationResult::Valid
                    ) {
                        // Apply key + switch provider in one step
                        let mut cfg = self.runtime.config().clone();
                        cfg.set_provider_api_key(provider, &api_key);
                        cfg.set_default_provider(provider);
                        if let Err(e) = self.runtime.apply_nca_config(cfg) {
                            tracing::warn!("onboarding: provider apply failed: {e}");
                            if let Ok(mut g) = tui_state.lock() {
                                g.validation_status =
                                    Some(crate::tui::state::OnboardingValidation::Failed(format!(
                                        "Failed to apply provider: {e}"
                                    )));
                                g.onboarding_mode = true;
                                g.mark_dirty();
                            }
                            continue;
                        }
                        // Sync TUI model display
                        if let Ok(mut g) = tui_state.lock() {
                            g.model = self.runtime.model().to_string();
                            g.mark_dirty();
                        }
                        // Persist only onboarding-owned provider fields globally;
                        // never serialize the fully merged runtime config.
                        let patch = ProviderConfigPatch {
                            provider,
                            set_default_provider: true,
                            api_key_env: Some(
                                self.runtime
                                    .config()
                                    .provider
                                    .api_key_env_for(provider)
                                    .to_string(),
                            ),
                            api_key: Some(api_key.clone()),
                            base_url: None,
                            model: None,
                            temperature: None,
                            compatibility: None,
                            onboarding_completed: Some(true),
                        };
                        if let Err(e) = self.runtime.config().save_provider_patch_global(&patch) {
                            tracing::warn!("onboarding: global config save failed: {e}");
                        }
                        let mut cfg = self.runtime.config().clone();
                        cfg.ui.onboarding_completed = true;
                        let _ = self.runtime.apply_nca_config(cfg);
                    }
                }
                TuiCmd::CompleteOnboarding => {
                    let mut cfg = self.runtime.config().clone();
                    cfg.ui.onboarding_completed = true;
                    let patch = ProviderConfigPatch {
                        provider: cfg.provider.default,
                        set_default_provider: false,
                        api_key_env: None,
                        api_key: None,
                        base_url: None,
                        model: None,
                        temperature: None,
                        compatibility: None,
                        onboarding_completed: Some(true),
                    };
                    if let Err(e) = cfg.save_provider_patch_global(&patch) {
                        tracing::warn!("onboarding flag save failed: {e}");
                    }
                    if let Err(e) = self.runtime.apply_nca_config(cfg) {
                        tracing::warn!("onboarding config apply failed: {e}");
                    }
                }
                TuiCmd::QuestionAnswer(selection) => {
                    let qid = if let Ok(g) = tui_state.lock() {
                        g.active_question.as_ref().map(|q| q.question_id.clone())
                    } else {
                        None
                    };
                    if let Some(qid) = qid
                        && !self.runtime.submit_question_answer(&qid, selection)
                        && let Ok(mut g) = tui_state.lock()
                    {
                        g.push_error(
                            "failed to submit answer (expired or already answered)".into(),
                        );
                    }
                }
                TuiCmd::Submit(line) => {
                    let line = line.trim().to_string();
                    let api_key_modal_state = tui_state.lock().ok().and_then(|g| {
                        g.api_key_modal_open().then_some((
                            g.api_key_target_provider(),
                            g.api_key_input().to_string(),
                            g.api_key_connect_after_save(),
                        ))
                    });
                    if let Some((Some(p), key_input, connect_after_save)) = api_key_modal_state {
                        let typed = if line.starts_with('/') {
                            ""
                        } else {
                            key_input.trim()
                        };
                        let had_existing = self.runtime.config().provider.api_key_present_for(p);
                        if line.starts_with('/') {
                            if let Ok(mut g) = tui_state.lock() {
                                g.close_api_key_modal();
                            }
                        } else if typed.is_empty() {
                            if had_existing {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.close_api_key_modal();
                                    g.blocks.push(DisplayBlock::System(format!(
                                        "[apikey] keeping existing key for {}",
                                        p.display_name()
                                    )));
                                    g.mark_transcript_dirty();
                                }
                                if connect_after_save {
                                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                                        .await?;
                                }
                            } else if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!(
                                    "[apikey] paste a key for {} or Esc to cancel",
                                    p.display_name()
                                ));
                            }
                            continue;
                        } else {
                            self.save_provider_api_key(p, typed, ReplOutput::Tui(&tui_state))
                                .await?;
                            if let Ok(mut g) = tui_state.lock() {
                                g.close_api_key_modal();
                            }
                            if connect_after_save {
                                self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                                    .await?;
                            }
                            continue;
                        }
                    }
                    if line.is_empty() {
                        if let Ok(mut g) = tui_state.lock()
                            && g.pending_api_key_provider.take().is_some()
                        {
                            g.blocks.push(DisplayBlock::System(
                                "[apikey] entry cancelled (empty line)".into(),
                            ));
                            g.mark_transcript_dirty();
                        }
                        continue;
                    }
                    if let Some(p) = tui_state
                        .lock()
                        .ok()
                        .and_then(|g| g.pending_api_key_provider)
                    {
                        if !line.starts_with('/') {
                            let mut cfg = self.runtime.config().clone();
                            cfg.set_provider_api_key(p, &line);
                            match self.runtime.apply_nca_config(cfg) {
                                Ok(()) => {
                                    if let Err(e) = self
                                        .runtime
                                        .config()
                                        .save_workspace_file(self.runtime.workspace_root())
                                    {
                                        if let Ok(mut g) = tui_state.lock() {
                                            g.push_error(format!(
                                                "[apikey] applied but save failed: {e}"
                                            ));
                                        }
                                    } else if let Ok(mut g) = tui_state.lock() {
                                        g.pending_api_key_provider = None;
                                        g.blocks.push(DisplayBlock::System(format!(
                                            "[apikey] saved for {}",
                                            p.display_name()
                                        )));
                                        g.mark_transcript_dirty();
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.push_error(format!("[apikey] {e}"));
                                    }
                                }
                            }
                            continue;
                        }
                        if let Ok(mut g) = tui_state.lock() {
                            g.pending_api_key_provider = None;
                            g.mark_dirty();
                        }
                    }
                    if line.starts_with('!') {
                        let shell_cmd = line.trim_start_matches('!').trim();
                        self.run_bash_tui(shell_cmd, &tui_state).await;
                        continue;
                    }
                    if line.starts_with('/') {
                        if !self
                            .handle_command(&line, ReplOutput::Tui(&tui_state))
                            .await?
                        {
                            if let Ok(mut g) = tui_state.lock() {
                                g.should_exit = true;
                                g.mark_dirty();
                            }
                            break;
                        }
                        continue;
                    }
                    let expanded =
                        match expand_at_file_mentions_default(&line, self.runtime.workspace_root())
                        {
                            Ok(s) => s,
                            Err(e) => {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("file mentions: {e}"));
                                }
                                continue;
                            }
                        };
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }
                    let attachments = if let Ok(mut g) = tui_state.lock() {
                        let attachments = std::mem::take(&mut g.staged_image_attachments);
                        g.mark_dirty();
                        attachments
                    } else {
                        Vec::new()
                    };
                    let turn = if attachments.is_empty() {
                        self.runtime.run_turn(&expanded).await
                    } else {
                        self.runtime
                            .run_turn_with_images(&expanded, attachments)
                            .await
                    };
                    if let Err(e) = turn
                        && let Ok(mut g) = tui_state.lock()
                    {
                        g.push_error(e.to_string());
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(false);
                    }
                }
            }
        }

        let _ = ui.await;
        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    async fn run_bash_tui(&self, cmd: &str, st: &Arc<Mutex<TuiSessionState>>) {
        fn log(st: &Arc<Mutex<TuiSessionState>>, s: &str) {
            if let Ok(mut g) = st.lock() {
                g.blocks.push(DisplayBlock::System(s.to_string()));
                g.mark_transcript_dirty();
            }
        }
        if cmd.is_empty() {
            log(st, "! usage: !<command>");
            return;
        }
        log(st, &format!("[bash] {cmd}"));
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stdout.is_empty()
                    && let Ok(mut g) = st.lock()
                {
                    for line in stdout.lines() {
                        g.blocks.push(DisplayBlock::System(line.to_string()));
                    }
                    g.mark_transcript_dirty();
                }
                if !stderr.is_empty() {
                    log(st, &format!("[stderr] {stderr}"));
                }
                log(
                    st,
                    &if out.status.success() {
                        "[bash] exit 0".into()
                    } else {
                        format!("[bash] exit {}", out.status.code().unwrap_or(-1))
                    },
                );
            }
            Err(e) => log(st, &format!("[bash] {e}")),
        }
    }
}

/// Tab completion for REPL commands and skills
impl Completer for Repl {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        if let Some((at_byte, prefix)) = at_token_before_cursor(line, pos) {
            let files = discover_workspace_files(self.runtime.workspace_root());
            for path in filter_paths_prefix(&files, &prefix) {
                suggestions.push(Suggestion {
                    value: format!("@{path}"),
                    description: Some("workspace file".to_string()),
                    extra: None,
                    span: reedline::Span {
                        start: at_byte,
                        end: pos,
                    },
                    append_whitespace: false,
                    style: None,
                });
            }
            if !suggestions.is_empty() {
                return suggestions;
            }
        }

        // Complete REPL commands starting with /
        if line.starts_with('/') {
            for spec in visible_commands() {
                if spec.name.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: spec.name.to_string(),
                        description: Some(spec.description.to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Complete bash mode commands (starting with !)
        if line.starts_with('!') {
            // Common shell commands
            let bash_commands = [
                "git", "ls", "cat", "find", "grep", "npm", "cargo", "make", "docker", "curl",
            ];
            let _prefix = line.trim_start_matches('!');
            for cmd in bash_commands {
                let full = format!("!{}", cmd);
                if full.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: full,
                        description: Some("Shell command".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Load skills for completion
        if let Ok(skills) = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        ) {
            for skill in skills {
                let skill_cmd = format!("/{}", skill.command);
                if skill_cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: skill_cmd,
                        description: skill.description,
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        suggestions
    }
}

fn build_model_picker_entries(
    config: &nca_common::config::NcaConfig,
    provider_models: &[String],
) -> Vec<ModelPickerEntry> {
    let mut entries = Vec::new();
    entries.push(ModelPickerEntry {
        label: "Providers".into(),
        detail: String::new(),
        action: ModelPickerAction::ApplyModel(String::new()),
        is_header: true,
    });
    for p in ProviderKind::ALL {
        let model = config.provider.model_for(p);
        let key_status = if config.provider.api_key_present_for(p) {
            "key ✓"
        } else {
            "no key"
        };
        let selected = if p == config.provider.default {
            " [active]"
        } else {
            ""
        };
        entries.push(ModelPickerEntry {
            label: format!("{}{}", p.display_name(), selected),
            detail: format!("{model} ({key_status})"),
            action: ModelPickerAction::SwitchProvider(p),
            is_header: false,
        });
    }

    if !provider_models.is_empty() {
        entries.push(ModelPickerEntry {
            label: format!("{} models", config.provider.default.display_name()),
            detail: String::new(),
            action: ModelPickerAction::ApplyModel(String::new()),
            is_header: true,
        });
        for model_id in provider_models {
            entries.push(ModelPickerEntry {
                label: model_id.clone(),
                detail: String::new(),
                action: ModelPickerAction::ApplyModel(model_id.clone()),
                is_header: false,
            });
        }
    }

    entries.push(ModelPickerEntry {
        label: "Aliases".into(),
        detail: String::new(),
        action: ModelPickerAction::ApplyModel(String::new()),
        is_header: true,
    });
    for (alias, target) in &config.model.aliases {
        entries.push(ModelPickerEntry {
            label: alias.clone(),
            detail: format!("→ {target}"),
            action: ModelPickerAction::ApplyModel(alias.clone()),
            is_header: false,
        });
    }
    entries
}

fn permission_mode_index(mode: PermissionMode) -> usize {
    match mode {
        PermissionMode::Default => 0,
        PermissionMode::Plan => 1,
        PermissionMode::AcceptEdits => 2,
        PermissionMode::DontAsk => 3,
        PermissionMode::BypassPermissions => 4,
    }
}

fn permission_mode_from_index(idx: usize) -> PermissionMode {
    match idx {
        0 => PermissionMode::Default,
        1 => PermissionMode::Plan,
        2 => PermissionMode::AcceptEdits,
        3 => PermissionMode::DontAsk,
        4 => PermissionMode::BypassPermissions,
        _ => PermissionMode::Default,
    }
}

fn parse_permission_mode(raw: &str) -> Option<PermissionMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "default" => Some(PermissionMode::Default),
        "plan" => Some(PermissionMode::Plan),
        "accept-edits" | "accept_edits" | "acceptedits" => Some(PermissionMode::AcceptEdits),
        "dont-ask" | "dont_ask" | "dontask" => Some(PermissionMode::DontAsk),
        "bypass-permissions" | "bypass_permissions" | "bypasspermissions" => {
            Some(PermissionMode::BypassPermissions)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::build_session_runtime;
    use crate::tui::input::{ConnectModalKeyResult, handle_connect_modal_key};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    static NEXT_TEST_RUNTIME_ID: AtomicU64 = AtomicU64::new(0);

    async fn test_repl(
        configure_custom: bool,
    ) -> (Repl, Arc<Mutex<TuiSessionState>>, tempfile::TempDir) {
        let workspace = tempfile::tempdir().expect("temporary workspace");
        let mut config = NcaConfig::default();
        config.provider.minimax.api_key = Some("test-minimax-key".into());
        if configure_custom {
            config.provider.custom.base_url = "https://gateway.example/v1".into();
            config.provider.custom.api_key = Some("test-custom-key".into());
            config.provider.custom.model = "gateway-model".into();
        }
        let runtime = build_session_runtime(
            config,
            workspace.path(),
            false,
            false,
            false,
            Some(format!(
                "repl-test-{}-{}",
                std::process::id(),
                NEXT_TEST_RUNTIME_ID.fetch_add(1, Ordering::Relaxed)
            )),
            None,
            None,
        )
        .await
        .expect("test runtime");
        let state = Arc::new(Mutex::new(TuiSessionState::new(
            runtime.session_id().to_string(),
            runtime.model().to_string(),
            "@build".into(),
            "default".into(),
            PathBuf::from(workspace.path()),
        )));
        (Repl::new(runtime, false, false), state, workspace)
    }

    #[test]
    fn parses_permission_aliases() {
        assert_eq!(
            parse_permission_mode("accept-edits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            parse_permission_mode("dontask"),
            Some(PermissionMode::DontAsk)
        );
        assert_eq!(
            parse_permission_mode("bypass_permissions"),
            Some(PermissionMode::BypassPermissions)
        );
        assert_eq!(parse_permission_mode("invalid"), None);
    }

    #[test]
    fn custom_provider_activation_requires_a_complete_resolvable_config() {
        let mut config = NcaConfig::default();
        assert!(Repl::custom_provider_setup_required(&config));

        config.provider.custom.base_url = "https://gateway.example".into();
        config.provider.custom.model = "gateway-model".into();
        config.provider.custom.api_key = Some("inline-secret".into());
        assert!(!Repl::custom_provider_setup_required(&config));

        config.provider.custom.api_key = None;
        config.provider.custom.api_key_env = "not-portable".into();
        assert!(Repl::custom_provider_setup_required(&config));
    }

    #[test]
    fn legacy_custom_command_parser_preserves_optional_fields() {
        assert_eq!(
            parse_custom_provider_command(
                "anthropic https://gateway.example/v1 pasted-secret gateway-model"
            ),
            Ok(Some(CustomProviderCommand {
                compatibility: ProviderCompatibility::Anthropic,
                base_url: "https://gateway.example/v1".into(),
                api_key: Some("pasted-secret".into()),
                model: Some("gateway-model".into()),
            }))
        );
        assert_eq!(
            parse_custom_provider_command("openai https://gateway.example"),
            Ok(Some(CustomProviderCommand {
                compatibility: ProviderCompatibility::OpenAi,
                base_url: "https://gateway.example".into(),
                api_key: None,
                model: None,
            }))
        );
        assert_eq!(
            parse_custom_provider_command(
                "responses https://gateway.example responses-secret responses-model",
            ),
            Ok(Some(CustomProviderCommand {
                compatibility: ProviderCompatibility::OpenAiResponses,
                base_url: "https://gateway.example".into(),
                api_key: Some("responses-secret".into()),
                model: Some("responses-model".into()),
            }))
        );
        assert_eq!(
            parse_custom_provider_command("nope https://gateway.example"),
            Err("compatibility must be `openai`, `responses`, or `anthropic`")
        );
    }

    #[test]
    fn model_picker_exposes_custom_as_a_provider_switch_action() {
        let config = NcaConfig::default();
        let entries = build_model_picker_entries(&config, &[]);
        let custom = entries
            .iter()
            .find(|entry| entry.label == "Custom")
            .expect("custom provider entry");

        assert!(!custom.is_header);
        assert!(matches!(
            &custom.action,
            ModelPickerAction::SwitchProvider(ProviderKind::Custom)
        ));
        assert!(custom.detail.contains("no key"));
    }

    #[tokio::test]
    async fn repl_connect_command_and_custom_selection_open_setup() {
        let (mut repl, state, _workspace) = test_repl(false).await;

        repl.handle_command("/connect", ReplOutput::Tui(&state))
            .await
            .expect("connect command");
        {
            let mut state = state.lock().expect("state lock");
            assert!(state.connect_modal_open());
            state.connect_search_mut().unwrap().push_str("custom");
            assert_eq!(
                handle_connect_modal_key(
                    &mut state,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                ),
                ConnectModalKeyResult::ConfigureCustom
            );
            state.open_custom_provider_setup_from_config(&repl.runtime.config().provider.custom);
            assert!(state.custom_provider_setup_open());
            assert_eq!(
                state.custom_provider_setup_step(),
                crate::tui::state::CustomProviderSetupStep::Compatibility
            );
        }
    }

    #[tokio::test]
    async fn repl_provider_custom_recovers_unconfigured_and_activates_configured() {
        let (mut repl, state, _workspace) = test_repl(false).await;
        repl.handle_command("/provider custom", ReplOutput::Tui(&state))
            .await
            .expect("unconfigured provider command");
        assert!(
            state
                .lock()
                .expect("state lock")
                .custom_provider_setup_open()
        );
        assert_eq!(
            repl.runtime.config().provider.default,
            ProviderKind::MiniMax
        );

        let entries = build_model_picker_entries(repl.runtime.config(), &[]);
        let custom = entries
            .iter()
            .find(|entry| entry.label == "Custom")
            .expect("custom model-picker row");
        assert_eq!(
            custom.action,
            ModelPickerAction::SwitchProvider(ProviderKind::Custom)
        );
        state.lock().expect("state lock").close_overlay();
        repl.apply_provider_in_session(ProviderKind::Custom, ReplOutput::Tui(&state))
            .await
            .expect("model-picker provider action");
        assert!(
            state
                .lock()
                .expect("state lock")
                .custom_provider_setup_open()
        );

        let (mut repl, state, workspace) = test_repl(true).await;
        repl.handle_command("/provider custom", ReplOutput::Tui(&state))
            .await
            .expect("configured provider command");
        assert_eq!(repl.runtime.config().provider.default, ProviderKind::Custom);
        assert_eq!(repl.runtime.model(), "gateway-model");
        assert_eq!(
            state
                .lock()
                .expect("state lock")
                .custom_provider_host
                .as_deref(),
            Some("gateway.example")
        );
        assert!(workspace.path().join(".nca/config.local.toml").exists());
    }

    #[tokio::test]
    async fn repl_provider_add_custom_is_explicit_editing_entry_point() {
        let (mut repl, state, _workspace) = test_repl(true).await;
        repl.handle_command("/provider add-custom", ReplOutput::Tui(&state))
            .await
            .expect("custom wizard command");
        let state = state.lock().expect("state lock");
        assert!(state.custom_provider_setup_open());
        assert_eq!(state.custom_setup_base_url(), "https://gateway.example/v1");
        assert_eq!(state.custom_setup_model_hint(), "gateway-model");
        assert!(state.custom_setup_api_key().is_empty());
    }

    #[tokio::test]
    async fn custom_setup_activation_failure_retains_previous_provider() {
        let (mut repl, state, _workspace) = test_repl(false).await;
        let previous_model = repl.runtime.model().to_string();

        repl.finish_custom_setup_submission(
            CustomProviderSetupSubmission {
                compatibility: ProviderCompatibility::OpenAi,
                base_url: "https://gateway.example".into(),
                api_key_env: "CUSTOM_PROVIDER_API_KEY".into(),
                api_key: Some("invalid\nsecret".into()),
                model: "gateway-model".into(),
            },
            &state,
            false,
        )
        .await
        .expect("activation failure is reported through the setup state");

        assert_eq!(
            repl.runtime.config().provider.default,
            ProviderKind::MiniMax
        );
        assert_eq!(repl.runtime.model(), previous_model);
        assert!(repl.runtime.config().provider.custom.base_url.is_empty());

        let state = state.lock().expect("state lock");
        assert!(state.custom_provider_setup_open());
        assert!(state.blocks.iter().any(|block| matches!(
            block,
            DisplayBlock::ErrorLine(message)
                if message.contains("custom provider activation failed")
        )));
    }

    #[tokio::test]
    async fn custom_setup_persistence_failure_retains_new_active_provider_and_warns() {
        let (mut repl, state, workspace) = test_repl(false).await;
        std::fs::write(workspace.path().join(".nca"), "not a directory")
            .expect("block workspace config directory");

        repl.finish_custom_setup_submission(
            CustomProviderSetupSubmission {
                compatibility: ProviderCompatibility::OpenAi,
                base_url: "https://gateway.example/v1".into(),
                api_key_env: "CUSTOM_PROVIDER_API_KEY".into(),
                api_key: Some("new-custom-secret".into()),
                model: "gateway-model".into(),
            },
            &state,
            false,
        )
        .await
        .expect("persistence failure is reported through the setup state");

        assert_eq!(repl.runtime.config().provider.default, ProviderKind::Custom);
        assert_eq!(
            repl.runtime.config().provider.custom.base_url,
            "https://gateway.example/v1"
        );
        assert_eq!(repl.runtime.config().provider.custom.model, "gateway-model");
        assert_eq!(repl.runtime.model(), "gateway-model");

        let state = state.lock().expect("state lock");
        assert!(!state.custom_provider_setup_open());
        assert_eq!(
            state.custom_provider_host.as_deref(),
            Some("gateway.example")
        );
        assert!(state.blocks.iter().any(|block| matches!(
            block,
            DisplayBlock::System(message)
                if message.contains("provider active; workspace save failed")
        )));
    }

    #[tokio::test]
    async fn legacy_custom_command_normalizes_and_persists_without_full_config_save() {
        let (mut repl, state, workspace) = test_repl(false).await;
        repl.handle_command(
            "/custom openai https://legacy.example/v1 pasted-secret legacy-model",
            ReplOutput::Tui(&state),
        )
        .await
        .expect("legacy custom command");

        assert_eq!(repl.runtime.config().provider.default, ProviderKind::Custom);
        assert_eq!(
            repl.runtime.config().provider.custom.base_url,
            "https://legacy.example/v1"
        );
        assert_eq!(repl.runtime.config().provider.custom.model, "legacy-model");
        let saved = std::fs::read_to_string(workspace.path().join(".nca/config.local.toml"))
            .expect("workspace config");
        assert!(saved.contains("legacy-model"));
        assert!(!saved.contains("test-minimax-key"));
    }

    #[tokio::test]
    async fn reasoning_effort_command_sets_and_persists_trimmed_value() {
        let (mut repl, state, workspace) = test_repl(false).await;

        repl.handle_command("/reasoning-effort   low  ", ReplOutput::Tui(&state))
            .await
            .expect("reasoning effort command");

        assert_eq!(repl.runtime.config().model.reasoning_effort, "low");
        let saved = std::fs::read_to_string(workspace.path().join(".nca/config.local.toml"))
            .expect("workspace config");
        assert!(saved.contains("reasoning_effort = \"low\""));

        repl.handle_command("/reasoning-effort nil", ReplOutput::Tui(&state))
            .await
            .expect("disable reasoning effort");
        assert_eq!(repl.runtime.config().model.reasoning_effort, "nil");
        let saved = std::fs::read_to_string(workspace.path().join(".nca/config.local.toml"))
            .expect("workspace config after disabling");
        assert!(saved.contains("reasoning_effort = \"nil\""));
    }

    #[tokio::test]
    async fn reasoning_effort_command_without_value_displays_current_setting() {
        let (mut repl, state, _) = test_repl(false).await;
        repl.runtime.config_mut().model.reasoning_effort = "high".into();

        repl.handle_command("/reasoning-effort", ReplOutput::Tui(&state))
            .await
            .expect("display reasoning effort");

        let state = state.lock().expect("state lock");
        assert!(state.blocks.iter().any(|block| matches!(
            block,
            DisplayBlock::System(message) if message.contains("reasoning effort: high")
        )));
    }

    #[tokio::test]
    async fn config_status_identifies_reasoning_effort_protocol_scope() {
        let (mut repl, state, _) = test_repl(false).await;
        repl.runtime.config_mut().model.reasoning_effort = "medium".into();

        repl.handle_command("/config", ReplOutput::Tui(&state))
            .await
            .expect("show config");

        let state = state.lock().expect("state lock");
        assert!(state.info_modal_open());
        assert!(state.info_modal_lines().iter().any(|line| {
            line.contains("Reasoning:")
                && line.contains("OpenAI-compatible only")
                && line.contains("inactive for active provider")
        }));
    }
}
