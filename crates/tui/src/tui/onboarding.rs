//! Standalone first-run onboarding TUI.
//!
//! Runs before the session runtime is created, so it has no provider or
//! supervisor. Its only job is to collect and validate an API key, then
//! persist it to the global config.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use nca_common::config::{
    CustomProviderConfig, NcaConfig, ProviderCompatibility, ProviderConfigPatch, ProviderKind,
};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::connect_modal::{ConnectRow, build_connect_rows, selectable_row_indices};
use super::custom_provider_flow::{
    CustomProviderProbeOutcome, CustomProviderSetupFlow, CustomProviderSetupTransition,
};
use super::state::OnboardingValidation;
use super::terminal::{restore_terminal, setup_terminal};

/// Shared validation state updated by the background task.
type ValidationState = Arc<Mutex<Option<OnboardingValidation>>>;

/// The standalone custom-provider setup steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomOnboardingStep {
    Compatibility,
    Endpoint,
    Credentials,
    Model,
}

/// The observable screen/state of first-run onboarding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingScreen {
    Connect,
    ApiKey(ProviderKind),
    Custom(CustomOnboardingStep),
    Probing,
    ProbeFailed { message: String, retryable: bool },
    Complete,
}

/// Result of an external custom-provider probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingProbeOutcome {
    Succeeded,
    RetryableFailure(String),
    HardFailure(String),
}

type CustomProbeState = Arc<Mutex<Option<(u64, OnboardingProbeOutcome)>>>;

fn spawn_custom_probe(config: CustomProviderConfig, generation: u64, state: CustomProbeState) {
    tokio::spawn(async move {
        let outcome = match nca_core::provider::custom::probe_custom_provider(&config).await {
            Ok(()) => OnboardingProbeOutcome::Succeeded,
            Err(nca_core::provider::ProviderError::Configuration(message)) => {
                OnboardingProbeOutcome::HardFailure(message)
            }
            Err(error) => OnboardingProbeOutcome::RetryableFailure(error.to_string()),
        };
        if let Ok(mut slot) = state.lock() {
            *slot = Some((generation, outcome));
        }
    });
}

/// Result returned by the testable onboarding state machine.
#[derive(Debug)]
// Keep the established public transition payload shape; boxing this variant
// would change the API for callers that destructure completed transitions.
#[allow(clippy::large_enum_variant)]
pub enum OnboardingTransition {
    Continue,
    Probe(CustomProviderConfig),
    Completed(OnboardingCompletion),
    Cancelled,
    Blocked(String),
}

/// The configuration produced after onboarding completes.
#[derive(Debug)]
pub struct OnboardingCompletion {
    pub config: NcaConfig,
    pub warning: Option<String>,
}

/// Pure onboarding transitions plus the global persistence boundary.
pub struct OnboardingFlow {
    config: NcaConfig,
    screen: OnboardingScreen,
    setup: CustomProviderSetupFlow,
}

impl OnboardingFlow {
    pub fn new(config: NcaConfig) -> Self {
        Self {
            setup: CustomProviderSetupFlow::new(&config.provider.custom),
            config,
            screen: OnboardingScreen::Connect,
        }
    }

    pub fn screen(&self) -> &OnboardingScreen {
        &self.screen
    }

    pub fn config(&self) -> &NcaConfig {
        &self.config
    }

    pub fn select_provider(&mut self, provider: ProviderKind) -> OnboardingTransition {
        self.screen = if provider == ProviderKind::Custom {
            self.setup = CustomProviderSetupFlow::new(&self.config.provider.custom);
            OnboardingScreen::Custom(CustomOnboardingStep::Compatibility)
        } else {
            OnboardingScreen::ApiKey(provider)
        };
        OnboardingTransition::Continue
    }

    pub fn choose_custom_compatibility(
        &mut self,
        compatibility: ProviderCompatibility,
    ) -> OnboardingTransition {
        if !matches!(
            self.screen,
            OnboardingScreen::Custom(CustomOnboardingStep::Compatibility)
        ) {
            return OnboardingTransition::Blocked(
                "custom compatibility is not being selected".into(),
            );
        }
        match self.setup.choose_compatibility(compatibility) {
            CustomProviderSetupTransition::Continue => {
                self.screen = OnboardingScreen::Custom(CustomOnboardingStep::Endpoint);
                OnboardingTransition::Continue
            }
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom compatibility transition failed".into()),
        }
    }

    pub fn submit_custom_endpoint(&mut self, endpoint: impl Into<String>) -> OnboardingTransition {
        if !matches!(
            self.screen,
            OnboardingScreen::Custom(CustomOnboardingStep::Endpoint)
        ) {
            return OnboardingTransition::Blocked("custom endpoint is not being entered".into());
        }
        match self.setup.submit_base_url(endpoint) {
            CustomProviderSetupTransition::Continue => {
                self.screen = OnboardingScreen::Custom(CustomOnboardingStep::Credentials);
                OnboardingTransition::Continue
            }
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom endpoint transition failed".into()),
        }
    }

    pub fn submit_custom_credentials(
        &mut self,
        api_key_env: impl Into<String>,
        api_key: impl Into<String>,
    ) -> OnboardingTransition {
        if !matches!(
            self.screen,
            OnboardingScreen::Custom(CustomOnboardingStep::Credentials)
        ) {
            return OnboardingTransition::Blocked(
                "custom credentials are not being entered".into(),
            );
        }
        match self.setup.submit_credentials(api_key_env, api_key) {
            CustomProviderSetupTransition::Continue => {
                self.screen = OnboardingScreen::Custom(CustomOnboardingStep::Model);
                OnboardingTransition::Continue
            }
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom credential transition failed".into()),
        }
    }

    pub fn update_custom_api_key_env(
        &mut self,
        api_key_env: impl Into<String>,
    ) -> OnboardingTransition {
        match self.setup.update_api_key_env(api_key_env) {
            CustomProviderSetupTransition::Continue => OnboardingTransition::Continue,
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom credential transition failed".into()),
        }
    }

    pub fn submit_custom_model(&mut self, model: impl Into<String>) -> OnboardingTransition {
        if !matches!(
            self.screen,
            OnboardingScreen::Custom(CustomOnboardingStep::Model)
        ) {
            return OnboardingTransition::Blocked("custom model is not being entered".into());
        }
        match self.setup.submit_model(model) {
            CustomProviderSetupTransition::Probe(custom) => {
                self.screen = OnboardingScreen::Probing;
                OnboardingTransition::Probe(custom)
            }
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom model transition failed".into()),
        }
    }

    pub fn apply_probe_outcome(&mut self, outcome: OnboardingProbeOutcome) -> OnboardingTransition {
        if !matches!(self.screen, OnboardingScreen::Probing) {
            return OnboardingTransition::Blocked(
                "the custom provider probe is no longer active".into(),
            );
        }
        let outcome = match outcome {
            OnboardingProbeOutcome::Succeeded => CustomProviderProbeOutcome::Succeeded,
            OnboardingProbeOutcome::RetryableFailure(message) => {
                CustomProviderProbeOutcome::RetryableFailure(message)
            }
            OnboardingProbeOutcome::HardFailure(message) => {
                CustomProviderProbeOutcome::HardFailure(message)
            }
        };
        match self.setup.apply_probe_outcome(outcome) {
            CustomProviderSetupTransition::Completed(custom) => self.complete(custom),
            CustomProviderSetupTransition::ContinueWithMessage(message) => {
                self.screen = OnboardingScreen::ProbeFailed {
                    message,
                    retryable: true,
                };
                OnboardingTransition::Continue
            }
            CustomProviderSetupTransition::Blocked(message) => {
                self.screen = OnboardingScreen::ProbeFailed {
                    message: message.clone(),
                    retryable: false,
                };
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom probe transition failed".into()),
        }
    }

    pub fn retry_probe(&mut self) -> OnboardingTransition {
        match self.setup.retry_probe() {
            CustomProviderSetupTransition::Probe(custom) => {
                self.screen = OnboardingScreen::Probing;
                OnboardingTransition::Probe(custom)
            }
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom retry transition failed".into()),
        }
    }

    pub fn save_anyway(&mut self) -> OnboardingTransition {
        match self.setup.save_anyway() {
            CustomProviderSetupTransition::Completed(custom) => self.complete(custom),
            CustomProviderSetupTransition::Blocked(message) => {
                OnboardingTransition::Blocked(message)
            }
            _ => OnboardingTransition::Blocked("custom save transition failed".into()),
        }
    }

    pub fn cancel(&mut self) -> OnboardingTransition {
        self.screen = OnboardingScreen::Connect;
        self.setup.cancel();
        OnboardingTransition::Cancelled
    }

    fn complete(&mut self, custom: CustomProviderConfig) -> OnboardingTransition {
        self.config.provider.custom = custom.clone();
        self.config.set_default_provider(ProviderKind::Custom);
        self.config.ui.onboarding_completed = true;

        let patch = ProviderConfigPatch {
            provider: ProviderKind::Custom,
            set_default_provider: true,
            api_key_env: Some(custom.api_key_env.clone()),
            api_key: None,
            base_url: Some(custom.base_url.clone()),
            model: Some(custom.model.clone()),
            temperature: None,
            compatibility: Some(custom.compatibility),
            onboarding_completed: Some(true),
        };
        let patch = ProviderConfigPatch {
            api_key: custom.api_key.clone(),
            ..patch
        };
        let warning = self
            .config
            .save_provider_patch_global(&patch)
            .err()
            .map(|error| format!("global configuration could not be saved: {error}"));
        self.screen = OnboardingScreen::Complete;
        OnboardingTransition::Completed(OnboardingCompletion {
            config: self.config.clone(),
            warning,
        })
    }
}

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn onboarding_connect_rows(search: &str) -> Vec<ConnectRow> {
    build_connect_rows(search)
}

/// Runs the onboarding TUI. Returns the updated config with the validated key
/// and onboarding_completed = true, or an error if the user quits (Ctrl+C).
pub async fn run_onboarding(config: NcaConfig) -> anyhow::Result<NcaConfig> {
    let mut terminal = setup_terminal()?;
    let result = run_onboarding_inner(config, &mut terminal).await;
    restore_terminal();
    result
}

async fn run_onboarding_inner(
    mut config: NcaConfig,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
) -> anyhow::Result<NcaConfig> {
    let mut connect_open = true;
    let mut connect_search = String::new();
    let mut connect_index: usize = 0;

    let mut api_key_open = false;
    let mut api_key_input = String::new();
    let mut api_key_provider: Option<ProviderKind> = None;

    let mut custom_flow: Option<OnboardingFlow> = None;
    let mut custom_compatibility = ProviderCompatibility::OpenAi;
    let mut custom_input = String::new();
    let mut custom_api_key_env = config.provider.custom.api_key_env.clone();
    let mut custom_api_key = String::new();
    let mut custom_model = config.provider.custom.model.clone();
    let mut custom_credentials_secret = false;
    let mut custom_probe_action = 0usize;
    let mut custom_error: Option<String> = None;
    let custom_probe_state: CustomProbeState = Arc::new(Mutex::new(None));
    let mut custom_probe_generation = 0u64;

    // Validation runs in a background tokio task; result is polled via shared state.
    let validation_state: ValidationState = Arc::new(Mutex::new(None));
    let spinner_start = Instant::now();

    loop {
        // Poll validation result from background task
        let current_validation = validation_state.lock().ok().and_then(|g| g.clone());

        if let Some((generation, outcome)) = custom_probe_state
            .lock()
            .ok()
            .and_then(|mut state| state.take())
            && generation == custom_probe_generation
            && let Some(flow) = custom_flow.as_mut()
        {
            match flow.apply_probe_outcome(outcome) {
                OnboardingTransition::Completed(completion) => {
                    if let Some(warning) = completion.warning {
                        eprintln!("Warning: {warning}");
                    }
                    return Ok(completion.config);
                }
                OnboardingTransition::Blocked(message) => custom_error = Some(message),
                OnboardingTransition::Continue | OnboardingTransition::Probe(_) => {}
                OnboardingTransition::Cancelled => {}
            }
        }

        // Check if validation succeeded — save and exit
        if let Some(OnboardingValidation::Valid) = &current_validation {
            if let Some(provider) = api_key_provider {
                config.set_provider_api_key(provider, api_key_input.trim());
                config.set_default_provider(provider);
                config.ui.onboarding_completed = true;
                let patch = ProviderConfigPatch {
                    provider,
                    set_default_provider: true,
                    api_key_env: Some(config.provider.api_key_env_for(provider).to_string()),
                    api_key: Some(api_key_input.trim().to_string()),
                    base_url: None,
                    model: None,
                    temperature: None,
                    compatibility: None,
                    onboarding_completed: Some(true),
                };
                if let Err(e) = config.save_provider_patch_global(&patch) {
                    tracing::warn!("onboarding: failed to save global config: {e}");
                    eprintln!(
                        "Warning: config saved in memory but failed to write to disk: {e}\n\
                         You may need to re-enter your API key on next launch."
                    );
                }
            }
            return Ok(config);
        }

        let is_validating = matches!(&current_validation, Some(OnboardingValidation::Validating))
            || matches!(
                custom_flow.as_ref().map(|flow| flow.screen()),
                Some(OnboardingScreen::Probing)
            );
        let spinner_idx =
            (spinner_start.elapsed().as_millis() / 80) as usize % SPINNER_FRAMES.len();

        // Render
        terminal.draw(|f| {
            let area = f.area();

            let bg = Block::default().style(Style::default().bg(Color::Black));
            f.render_widget(bg, area);

            // Title
            let title = Paragraph::new("Welcome to nca — connect a provider to get started")
                .style(Style::default().fg(Color::White))
                .alignment(ratatui::layout::Alignment::Center);
            f.render_widget(
                title,
                Rect {
                    x: 0,
                    y: area.height / 4,
                    width: area.width,
                    height: 1,
                },
            );

            /*
            if custom_flow.is_some() {
                let mut close_custom = false;
                let flow = custom_flow.as_mut().expect("custom onboarding flow");
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        flow.cancel();
                        close_custom = true;
                        custom_error = None;
                        custom_step = CustomOnboardingStep::Compatibility;
                        custom_input.clear();
                        connect_open = true;
                    }
                    (KeyCode::Up, _) if custom_step == CustomOnboardingStep::Compatibility => {
                        custom_compatibility = ProviderCompatibility::from_index(
                            custom_compatibility.index().saturating_sub(1),
                        );
                    }
                    (KeyCode::Down, _) if custom_step == CustomOnboardingStep::Compatibility => {
                        custom_compatibility = ProviderCompatibility::from_index(
                            (custom_compatibility.index() + 1)
                                .min(ProviderCompatibility::ALL.len() - 1),
                        );
                    }
                    (KeyCode::Enter, _) if custom_step == CustomOnboardingStep::Compatibility => {
                        let result = flow.choose_custom_compatibility(custom_compatibility);
                        if let OnboardingTransition::Continue = result {
                            custom_input = config.provider.custom.base_url.clone();
                            custom_error = None;
                        }
                    }
                    (KeyCode::Enter, _) if custom_step == CustomOnboardingStep::Endpoint => {
                        match flow.submit_custom_endpoint(custom_input.clone()) {
                            OnboardingTransition::Continue => {
                                custom_step = CustomOnboardingStep::Credentials;
                                custom_credentials_secret = false;
                                custom_input = custom_api_key_env.clone();
                                custom_error = None;
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            _ => {}
                        }
                    }
                    (KeyCode::Enter, _)
                        if custom_step == CustomOnboardingStep::Credentials
                            && !custom_credentials_secret =>
                    {
                        custom_api_key_env = custom_input.trim().to_string();
                        if custom_api_key_env.is_empty() {
                            custom_error = Some("API-key environment variable is required".into());
                        } else {
                            custom_credentials_secret = true;
                            custom_input = custom_api_key.clone();
                            custom_error = None;
                        }
                    }
                    (KeyCode::Enter, _)
                        if custom_step == CustomOnboardingStep::Credentials
                            && custom_credentials_secret =>
                    {
                        custom_api_key = custom_input.clone();
                        match flow.submit_custom_credentials(
                            custom_api_key_env.clone(),
                            custom_api_key.clone(),
                        ) {
                            OnboardingTransition::Continue => {
                                custom_step = CustomOnboardingStep::Model;
                                custom_input = custom_model.clone();
                                custom_error = None;
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            _ => {}
                        }
                    }
                    (KeyCode::Enter, _) if custom_step == CustomOnboardingStep::Model => {
                        custom_model = custom_input.clone();
                        match flow.submit_custom_model(custom_model.clone()) {
                            OnboardingTransition::Probe(custom) => {
                                custom_error = None;
                                if let Ok(mut state) = custom_probe_state.lock() {
                                    *state = None;
                                }
                                let probe_state = custom_probe_state.clone();
                                tokio::spawn(async move {
                                    let result =
                                        nca_core::provider::custom::probe_custom_provider(&custom)
                                            .await;
                                    let outcome = match result {
                                        Ok(()) => OnboardingProbeOutcome::Succeeded,
                                        Err(nca_core::provider::ProviderError::Configuration(
                                            message,
                                        )) => OnboardingProbeOutcome::HardFailure(message),
                                        Err(error) => OnboardingProbeOutcome::RetryableFailure(
                                            error.to_string(),
                                        ),
                                    };
                                    if let Ok(mut state) = probe_state.lock() {
                                        *state = Some(outcome);
                                    }
                                });
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            _ => {}
                        }
                    }
                    (KeyCode::Up, _)
                        if matches!(flow.screen(), OnboardingScreen::ProbeFailed { .. }) =>
                    {
                        custom_probe_action = custom_probe_action.saturating_sub(1);
                    }
                    (KeyCode::Down, _)
                        if matches!(flow.screen(), OnboardingScreen::ProbeFailed { .. }) =>
                    {
                        custom_probe_action = (custom_probe_action + 1).min(2);
                    }
                    (KeyCode::Enter, _)
                        if matches!(flow.screen(), OnboardingScreen::ProbeFailed { .. }) =>
                    {
                        let result = match custom_probe_action {
                            0 => flow.retry_probe(),
                            1 => flow.save_anyway(),
                            _ => {
                                flow.cancel();
                                close_custom = true;
                                OnboardingTransition::Cancelled
                            }
                        };
                        match result {
                            OnboardingTransition::Probe(custom) => {
                                if let Ok(mut state) = custom_probe_state.lock() {
                                    *state = None;
                                }
                                let probe_state = custom_probe_state.clone();
                                tokio::spawn(async move {
                                    let outcome =
                                        match nca_core::provider::custom::probe_custom_provider(
                                            &custom,
                                        )
                                        .await
                                        {
                                            Ok(()) => OnboardingProbeOutcome::Succeeded,
                                            Err(
                                                nca_core::provider::ProviderError::Configuration(
                                                    message,
                                                ),
                                            ) => OnboardingProbeOutcome::HardFailure(message),
                                            Err(error) => OnboardingProbeOutcome::RetryableFailure(
                                                error.to_string(),
                                            ),
                                        };
                                    if let Ok(mut state) = probe_state.lock() {
                                        *state = Some(outcome);
                                    }
                                });
                            }
                            OnboardingTransition::Completed(completion) => {
                                if let Some(warning) = completion.warning {
                                    eprintln!("Warning: {warning}");
                                }
                                return Ok(completion.config);
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            OnboardingTransition::Continue | OnboardingTransition::Cancelled => {}
                        }
                    }
                    (KeyCode::Backspace, _)
                        if matches!(
                            custom_step,
                            CustomOnboardingStep::Endpoint
                                | CustomOnboardingStep::Credentials
                                | CustomOnboardingStep::Model
                        ) =>
                    {
                        custom_input.pop();
                        custom_error = None;
                    }
                    (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT)
                        if matches!(
                            custom_step,
                            CustomOnboardingStep::Endpoint
                                | CustomOnboardingStep::Credentials
                                | CustomOnboardingStep::Model
                        ) =>
                    {
                        custom_input.push(c);
                        custom_error = None;
                    }
                    _ => {}
                }
                if close_custom {
                    custom_flow = None;
                    connect_open = true;
                }
            } else if api_key_open {
                render_api_key_modal(
                    f,
                    area,
                    api_key_provider,
                    &api_key_input,
                    &current_validation,
                    spinner_idx,
                );
            } else if connect_open {
                render_connect_modal(f, area, &connect_search, connect_index);
            }
            */
            if let Some(flow) = custom_flow.as_ref() {
                render_custom_provider_modal(
                    f,
                    area,
                    CustomProviderModalProps {
                        screen: flow.screen(),
                        compatibility: custom_compatibility,
                        input: &custom_input,
                        api_key_env: &custom_api_key_env,
                        credentials_secret: custom_credentials_secret,
                        probe_action: custom_probe_action,
                        error: custom_error.as_deref(),
                        spinner_idx,
                    },
                );
            } else if api_key_open {
                render_api_key_modal(
                    f,
                    area,
                    api_key_provider,
                    &api_key_input,
                    &current_validation,
                    spinner_idx,
                );
            } else if connect_open {
                render_connect_modal(f, area, &connect_search, connect_index);
            }
        })?;

        // Poll events (short timeout so spinner animates smoothly)
        if event::poll(Duration::from_millis(if is_validating { 30 } else { 50 }))?
            && let Event::Key(key) = event::read()?
        {
            // Global quit
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                anyhow::bail!("onboarding cancelled by user");
            }

            if let Some(flow) = custom_flow.as_mut() {
                let screen = flow.screen().clone();
                let mut close_custom = false;
                match (key.code, key.modifiers, screen) {
                    (KeyCode::Esc, _, _) => {
                        flow.cancel();
                        custom_probe_generation = custom_probe_generation.wrapping_add(1);
                        close_custom = true;
                        custom_error = None;
                        connect_open = true;
                    }
                    (
                        KeyCode::Up,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Compatibility),
                    ) => {
                        custom_compatibility = ProviderCompatibility::from_index(
                            custom_compatibility.index().saturating_sub(1),
                        );
                    }
                    (
                        KeyCode::Down,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Compatibility),
                    ) => {
                        custom_compatibility = ProviderCompatibility::from_index(
                            (custom_compatibility.index() + 1)
                                .min(ProviderCompatibility::ALL.len() - 1),
                        );
                    }
                    (
                        KeyCode::Enter,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Compatibility),
                    ) => {
                        if matches!(
                            flow.choose_custom_compatibility(custom_compatibility),
                            OnboardingTransition::Continue
                        ) {
                            custom_input = config.provider.custom.base_url.clone();
                            custom_error = None;
                        }
                    }
                    (
                        KeyCode::Enter,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Endpoint),
                    ) => match flow.submit_custom_endpoint(custom_input.clone()) {
                        OnboardingTransition::Continue => {
                            custom_credentials_secret = false;
                            custom_input = custom_api_key_env.clone();
                            custom_error = None;
                        }
                        OnboardingTransition::Blocked(message) => custom_error = Some(message),
                        _ => {}
                    },
                    (
                        KeyCode::Enter,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Credentials),
                    ) if !custom_credentials_secret => {
                        match flow.update_custom_api_key_env(custom_input.trim()) {
                            OnboardingTransition::Continue => {
                                custom_api_key_env = custom_input.trim().to_string();
                                custom_credentials_secret = true;
                                custom_input = custom_api_key.clone();
                                custom_error = None;
                            }
                            OnboardingTransition::Blocked(error) => custom_error = Some(error),
                            _ => {}
                        }
                    }
                    (
                        KeyCode::Enter,
                        _,
                        OnboardingScreen::Custom(CustomOnboardingStep::Credentials),
                    ) => {
                        custom_api_key = custom_input.clone();
                        match flow.submit_custom_credentials(
                            custom_api_key_env.clone(),
                            custom_api_key.clone(),
                        ) {
                            OnboardingTransition::Continue => {
                                custom_input = custom_model.clone();
                                custom_error = None;
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            _ => {}
                        }
                    }
                    (KeyCode::Enter, _, OnboardingScreen::Custom(CustomOnboardingStep::Model)) => {
                        custom_model = custom_input.trim().to_string();
                        match flow.submit_custom_model(custom_model.clone()) {
                            OnboardingTransition::Probe(custom) => {
                                custom_error = None;
                                if let Ok(mut state) = custom_probe_state.lock() {
                                    *state = None;
                                }
                                spawn_custom_probe(
                                    custom,
                                    custom_probe_generation,
                                    custom_probe_state.clone(),
                                );
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            _ => {}
                        }
                    }
                    (KeyCode::Up, _, OnboardingScreen::ProbeFailed { .. }) => {
                        custom_probe_action = custom_probe_action.saturating_sub(1);
                    }
                    (KeyCode::Down, _, OnboardingScreen::ProbeFailed { .. }) => {
                        custom_probe_action = (custom_probe_action + 1).min(2);
                    }
                    (KeyCode::Enter, _, OnboardingScreen::ProbeFailed { .. }) => {
                        let transition = match custom_probe_action {
                            0 => flow.retry_probe(),
                            1 => flow.save_anyway(),
                            _ => flow.cancel(),
                        };
                        match transition {
                            OnboardingTransition::Probe(custom) => {
                                if let Ok(mut state) = custom_probe_state.lock() {
                                    *state = None;
                                }
                                spawn_custom_probe(
                                    custom,
                                    custom_probe_generation,
                                    custom_probe_state.clone(),
                                );
                            }
                            OnboardingTransition::Completed(completion) => {
                                if let Some(warning) = completion.warning {
                                    eprintln!("Warning: {warning}");
                                }
                                return Ok(completion.config);
                            }
                            OnboardingTransition::Cancelled => {
                                close_custom = true;
                                connect_open = true;
                                custom_error = None;
                            }
                            OnboardingTransition::Blocked(message) => custom_error = Some(message),
                            OnboardingTransition::Continue => {}
                        }
                    }
                    (KeyCode::Backspace, _, OnboardingScreen::Custom(_)) => {
                        custom_input.pop();
                        custom_error = None;
                    }
                    (
                        KeyCode::Char(c),
                        KeyModifiers::NONE | KeyModifiers::SHIFT,
                        OnboardingScreen::Custom(_),
                    ) => {
                        custom_input.push(c);
                        custom_error = None;
                    }
                    _ => {}
                }
                if close_custom {
                    custom_flow = None;
                }
            } else if api_key_open {
                // Block input while validating
                if is_validating {
                    continue;
                }

                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _) => {
                        api_key_open = false;
                        api_key_input.clear();
                        api_key_provider = None;
                        if let Ok(mut g) = validation_state.lock() {
                            *g = None;
                        }
                        connect_open = true;
                    }
                    (KeyCode::Enter, _) => {
                        if let Some(provider) = api_key_provider {
                            let key_str = api_key_input.trim().to_string();
                            if !key_str.is_empty() {
                                // Set validating state
                                if let Ok(mut g) = validation_state.lock() {
                                    *g = Some(OnboardingValidation::Validating);
                                }

                                // Spawn background validation task
                                let base_url = config.provider.base_url_for(provider).to_string();
                                let vs = validation_state.clone();
                                tokio::spawn(async move {
                                    let result = nca_core::provider::validate::validate_api_key(
                                        provider, &key_str, &base_url, None,
                                    )
                                    .await;
                                    if let Ok(mut g) = vs.lock() {
                                        *g = Some(match result {
                                            nca_core::provider::validate::ValidationResult::Valid => {
                                                OnboardingValidation::Valid
                                            }
                                            nca_core::provider::validate::ValidationResult::InvalidKey(msg) => {
                                                OnboardingValidation::Failed(msg)
                                            }
                                            nca_core::provider::validate::ValidationResult::NetworkError(msg) => {
                                                OnboardingValidation::Failed(msg)
                                            }
                                        });
                                    }
                                });
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        api_key_input.pop();
                        if let Ok(mut g) = validation_state.lock() {
                            *g = None;
                        }
                    }
                    (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                        api_key_input.push(c);
                        if let Ok(mut g) = validation_state.lock() {
                            *g = None;
                        }
                    }
                    _ => {}
                }
            } else if connect_open {
                let rows = onboarding_connect_rows(&connect_search);
                let sel_indices = selectable_row_indices(&rows);
                let n_sel = sel_indices.len();

                match (key.code, key.modifiers) {
                    (KeyCode::Up, _) => {
                        if n_sel > 0 {
                            connect_index = connect_index.saturating_sub(1);
                        }
                    }
                    (KeyCode::Down, _) => {
                        if n_sel > 0 {
                            connect_index = (connect_index + 1).min(n_sel - 1);
                        }
                    }
                    (KeyCode::Enter, _) => {
                        if let Some(&row_idx) = sel_indices.get(connect_index)
                            && let ConnectRow::Provider { kind, .. } = &rows[row_idx]
                        {
                            if *kind == ProviderKind::Custom {
                                custom_probe_generation = custom_probe_generation.wrapping_add(1);
                                if let Ok(mut state) = custom_probe_state.lock() {
                                    *state = None;
                                }
                                custom_flow = Some(OnboardingFlow::new(config.clone()));
                                if let Some(flow) = custom_flow.as_mut() {
                                    flow.select_provider(ProviderKind::Custom);
                                }
                                custom_compatibility = config.provider.custom.compatibility;
                                custom_input.clear();
                                custom_api_key_env = config.provider.custom.api_key_env.clone();
                                custom_api_key.clear();
                                custom_model = config.provider.custom.model.clone();
                                custom_credentials_secret = false;
                                custom_probe_action = 0;
                                custom_error = None;
                                connect_open = false;
                            } else {
                                api_key_provider = Some(*kind);
                                api_key_open = true;
                                connect_open = false;
                                api_key_input.clear();
                                if let Ok(mut g) = validation_state.lock() {
                                    *g = None;
                                }
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        connect_search.pop();
                        connect_index = 0;
                    }
                    (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                        connect_search.push(c);
                        connect_index = 0;
                    }
                    _ => {}
                }
            }
        }
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

fn render_connect_modal(f: &mut Frame, area: Rect, search: &str, selected: usize) {
    let modal_rect = centered_rect(50, 14, area);
    f.render_widget(Clear, modal_rect);

    let block = Block::default()
        .title(" Connect a Provider ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray));
    let inner = block.inner(modal_rect);
    f.render_widget(block, modal_rect);

    let rows = onboarding_connect_rows(search);
    let sel_indices = selectable_row_indices(&rows);

    let mut lines = Vec::new();

    let search_display = if search.is_empty() {
        "Type to filter...".to_string()
    } else {
        search.to_string()
    };
    lines.push(Line::from(vec![
        Span::styled("/ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            search_display,
            Style::default().fg(if search.is_empty() {
                Color::DarkGray
            } else {
                Color::White
            }),
        ),
    ]));
    lines.push(Line::from(""));

    for (i, row) in rows.iter().enumerate() {
        match row {
            ConnectRow::SectionHeader(label) => {
                lines.push(Line::from(Span::styled(
                    format!("  {label}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
            ConnectRow::Provider {
                title, subtitle, ..
            } => {
                let is_selected = sel_indices.iter().position(|&si| si == i) == Some(selected);
                let style = if is_selected {
                    Style::default().fg(Color::Black).bg(Color::White)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if is_selected { "> " } else { "  " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{title} — {subtitle}"),
                    style,
                )));
            }
        }
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

struct CustomProviderModalProps<'a> {
    screen: &'a OnboardingScreen,
    compatibility: ProviderCompatibility,
    input: &'a str,
    api_key_env: &'a str,
    credentials_secret: bool,
    probe_action: usize,
    error: Option<&'a str>,
    spinner_idx: usize,
}

fn render_custom_provider_modal(f: &mut Frame, area: Rect, props: CustomProviderModalProps<'_>) {
    let CustomProviderModalProps {
        screen,
        compatibility,
        input,
        api_key_env,
        credentials_secret,
        probe_action,
        error,
        spinner_idx,
    } = props;
    let modal_rect = centered_rect(62, 16, area);
    f.render_widget(Clear, modal_rect);
    let block = Block::default()
        .title(" Custom provider setup ")
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray));
    let inner = block.inner(modal_rect);
    f.render_widget(block, modal_rect);

    let mut lines = Vec::new();
    match screen {
        OnboardingScreen::Custom(CustomOnboardingStep::Compatibility) => {
            lines.push(Line::from("Choose API compatibility:"));
            for option in ProviderCompatibility::ALL.into_iter() {
                let selected = option == compatibility;
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {}",
                        if selected { ">" } else { " " },
                        option.display_name()
                    ),
                    if selected {
                        Style::default().fg(Color::Black).bg(Color::White)
                    } else {
                        Style::default().fg(Color::White)
                    },
                )));
            }
            lines.push(Line::from("↑/↓ choose · Enter continue · Esc cancel"));
        }
        OnboardingScreen::Custom(CustomOnboardingStep::Endpoint) => {
            lines.push(Line::from("Base URL (origin or path ending in /v1):"));
            lines.push(Line::from(Span::styled(
                format!("{input}▌"),
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from("Enter continue · Esc cancel"));
        }
        OnboardingScreen::Custom(CustomOnboardingStep::Credentials) => {
            lines.push(Line::from("API-key environment variable:"));
            lines.push(Line::from(Span::styled(
                if credentials_secret {
                    api_key_env.to_string()
                } else {
                    format!("{input}▌")
                },
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(if credentials_secret {
                "API key (blank uses the environment):"
            } else {
                "Enter to continue to the API key"
            }));
            if credentials_secret {
                let masked = if input.is_empty() {
                    "(environment value)".to_string()
                } else {
                    format!("{}▌", "*".repeat(input.chars().count()))
                };
                lines.push(Line::from(Span::styled(
                    masked,
                    Style::default().fg(Color::Green),
                )));
            }
            lines.push(Line::from("Enter continue · Esc cancel"));
        }
        OnboardingScreen::Custom(CustomOnboardingStep::Model) => {
            lines.push(Line::from("Model id:"));
            lines.push(Line::from(Span::styled(
                format!("{input}▌"),
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from("Enter to probe · Esc cancel"));
        }
        OnboardingScreen::Probing => {
            let frame = SPINNER_FRAMES[spinner_idx];
            lines.push(Line::from(format!("{frame} Checking custom provider…")));
        }
        OnboardingScreen::ProbeFailed { message, retryable } => {
            lines.push(Line::from(Span::styled(
                format!("Probe failed: {message}"),
                Style::default().fg(Color::Red),
            )));
            if *retryable {
                for (index, action) in ["Retry", "Save anyway", "Cancel"].into_iter().enumerate() {
                    let selected = index == probe_action;
                    lines.push(Line::from(Span::styled(
                        format!("{} {action}", if selected { ">" } else { " " }),
                        if selected {
                            Style::default().fg(Color::Black).bg(Color::White)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    )));
                }
            } else {
                lines.push(Line::from(
                    "This configuration cannot be saved until it is fixed.",
                ));
                lines.push(Line::from("Press Esc to cancel."));
            }
        }
        OnboardingScreen::Connect | OnboardingScreen::ApiKey(_) | OnboardingScreen::Complete => {}
    }
    if let Some(error) = error {
        lines.push(Line::from(Span::styled(
            error,
            Style::default().fg(Color::Red),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn render_api_key_modal(
    f: &mut Frame,
    area: Rect,
    provider: Option<ProviderKind>,
    input: &str,
    validation: &Option<OnboardingValidation>,
    spinner_idx: usize,
) {
    let modal_rect = centered_rect(50, 10, area);
    f.render_widget(Clear, modal_rect);

    let provider_name = provider.map(|p| p.display_name()).unwrap_or("Provider");
    let block = Block::default()
        .title(format!(" API Key for {provider_name} "))
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::DarkGray));
    let inner = block.inner(modal_rect);
    f.render_widget(block, modal_rect);

    let masked: String = "*".repeat(input.len());
    let mut lines = vec![
        Line::from(Span::styled(
            "Paste your API key:",
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if masked.is_empty() { "..." } else { &masked },
            Style::default().fg(if masked.is_empty() {
                Color::DarkGray
            } else {
                Color::Green
            }),
        )),
        Line::from(""),
    ];

    match validation {
        Some(OnboardingValidation::Validating) => {
            let frame = SPINNER_FRAMES[spinner_idx];
            lines.push(Line::from(vec![
                Span::styled(format!("{frame} "), Style::default().fg(Color::Cyan)),
                Span::styled("Validating API key...", Style::default().fg(Color::Yellow)),
            ]));
        }
        Some(OnboardingValidation::Failed(msg)) => {
            lines.push(Line::from(Span::styled(
                msg.as_str(),
                Style::default().fg(Color::Red),
            )));
        }
        _ => {
            lines.push(Line::from(Span::styled(
                "Press Enter to validate | Esc to go back",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, MutexGuard};

    struct EnvGuard {
        previous: Option<String>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn nca_home(path: &std::path::Path) -> Self {
            static ENV_LOCK: Mutex<()> = Mutex::new(());
            let lock = ENV_LOCK.lock().expect("environment lock");
            let previous = env::var("NCA_HOME").ok();
            unsafe { env::set_var("NCA_HOME", path) };
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.as_deref() {
                Some(value) => unsafe { env::set_var("NCA_HOME", value) },
                None => unsafe { env::remove_var("NCA_HOME") },
            }
        }
    }

    fn configured_flow() -> OnboardingFlow {
        OnboardingFlow::new(NcaConfig::default())
    }

    fn reach_probe(flow: &mut OnboardingFlow) -> OnboardingTransition {
        assert!(matches!(
            flow.select_provider(ProviderKind::Custom),
            OnboardingTransition::Continue
        ));
        assert!(matches!(
            flow.choose_custom_compatibility(ProviderCompatibility::OpenAi),
            OnboardingTransition::Continue
        ));
        assert!(matches!(
            flow.submit_custom_endpoint("https://gateway.example/v1"),
            OnboardingTransition::Continue
        ));
        assert!(matches!(
            flow.submit_custom_credentials("GATEWAY_API_KEY", "secret"),
            OnboardingTransition::Continue
        ));
        flow.submit_custom_model("gateway-model")
    }

    #[test]
    fn onboarding_provider_choices_include_custom() {
        let rows = onboarding_connect_rows("");

        assert!(rows.iter().any(|row| matches!(
            row,
            ConnectRow::Provider {
                kind: ProviderKind::Custom,
                ..
            }
        )));
    }

    #[test]
    fn selecting_custom_enters_standalone_setup_steps() {
        let mut flow = configured_flow();

        flow.select_provider(ProviderKind::Custom);
        assert_eq!(
            flow.screen(),
            &OnboardingScreen::Custom(CustomOnboardingStep::Compatibility)
        );
        flow.choose_custom_compatibility(ProviderCompatibility::Anthropic);
        assert_eq!(
            flow.screen(),
            &OnboardingScreen::Custom(CustomOnboardingStep::Endpoint)
        );
        flow.submit_custom_endpoint("https://gateway.example");
        assert_eq!(
            flow.screen(),
            &OnboardingScreen::Custom(CustomOnboardingStep::Credentials)
        );
        flow.submit_custom_credentials("GATEWAY_API_KEY", "secret");
        assert_eq!(
            flow.screen(),
            &OnboardingScreen::Custom(CustomOnboardingStep::Model)
        );
    }

    #[test]
    fn completed_custom_setup_emits_probe_with_normalized_public_fields() {
        let mut flow = configured_flow();

        let transition = reach_probe(&mut flow);

        let OnboardingTransition::Probe(custom) = transition else {
            panic!("custom setup should start a probe");
        };
        assert_eq!(custom.base_url, "https://gateway.example/v1");
        assert_eq!(custom.api_key_env, "GATEWAY_API_KEY");
        assert_eq!(custom.model, "gateway-model");
        assert_eq!(custom.compatibility, ProviderCompatibility::OpenAi);
        assert_eq!(custom.api_key, Some("secret".into()));
        assert_eq!(flow.screen(), &OnboardingScreen::Probing);
    }

    #[test]
    fn retryable_probe_failure_offers_retry_and_save_anyway() {
        let mut flow = configured_flow();
        assert!(matches!(
            reach_probe(&mut flow),
            OnboardingTransition::Probe(_)
        ));

        assert!(matches!(
            flow.apply_probe_outcome(OnboardingProbeOutcome::RetryableFailure(
                "gateway unavailable".into()
            )),
            OnboardingTransition::Continue
        ));
        assert!(matches!(
            flow.screen(),
            OnboardingScreen::ProbeFailed {
                retryable: true,
                message
            } if message == "gateway unavailable"
        ));
        assert!(matches!(flow.retry_probe(), OnboardingTransition::Probe(_)));
    }

    #[test]
    fn hard_probe_failure_blocks_save_anyway() {
        let mut flow = configured_flow();
        assert!(matches!(
            reach_probe(&mut flow),
            OnboardingTransition::Probe(_)
        ));

        assert!(matches!(
            flow.apply_probe_outcome(OnboardingProbeOutcome::HardFailure("bad URL".into())),
            OnboardingTransition::Blocked(message) if message == "bad URL"
        ));
        assert!(matches!(
            flow.save_anyway(),
            OnboardingTransition::Blocked(message)
                if message.contains("retryable probe failure")
        ));
    }

    #[test]
    fn save_anyway_completes_onboarding_after_retryable_probe_failure() {
        let home = tempfile::tempdir().expect("temp home");
        let _env = EnvGuard::nca_home(home.path());
        let mut flow = configured_flow();
        assert!(matches!(
            reach_probe(&mut flow),
            OnboardingTransition::Probe(_)
        ));
        flow.apply_probe_outcome(OnboardingProbeOutcome::RetryableFailure("offline".into()));

        let OnboardingTransition::Completed(completion) = flow.save_anyway() else {
            panic!("save anyway should complete onboarding");
        };
        assert!(completion.warning.is_none());
        assert!(completion.config.ui.onboarding_completed);
        assert_eq!(completion.config.provider.default, ProviderKind::Custom);
    }

    #[test]
    fn successful_custom_setup_persists_global_provider_fields_only() {
        let home = tempfile::tempdir().expect("temp home");
        let _env = EnvGuard::nca_home(home.path());
        let mut flow = configured_flow();
        assert!(matches!(
            reach_probe(&mut flow),
            OnboardingTransition::Probe(_)
        ));

        let OnboardingTransition::Completed(completion) =
            flow.apply_probe_outcome(OnboardingProbeOutcome::Succeeded)
        else {
            panic!("successful probe should complete onboarding");
        };
        let raw = std::fs::read_to_string(home.path().join("config.toml")).expect("global config");
        assert!(raw.contains("default = \"custom\""));
        assert!(raw.contains("base_url = \"https://gateway.example/v1\""));
        assert!(raw.contains("model = \"gateway-model\""));
        assert!(raw.contains("onboarding_completed = true"));
        assert!(raw.contains("api_key_env = \"GATEWAY_API_KEY\""));
        assert!(raw.contains("api_key = \"secret\""));
        assert!(!raw.contains("[model]"));
        assert!(completion.config.ui.onboarding_completed);
    }

    #[test]
    fn cancellation_leaves_config_uncompleted_and_does_not_persist() {
        let home = tempfile::tempdir().expect("temp home");
        let _env = EnvGuard::nca_home(home.path());
        let mut flow = configured_flow();
        flow.select_provider(ProviderKind::Custom);

        assert!(matches!(flow.cancel(), OnboardingTransition::Cancelled));
        assert_eq!(flow.screen(), &OnboardingScreen::Connect);
        assert!(!flow.config().ui.onboarding_completed);
        assert!(!home.path().join("config.toml").exists());
    }

    #[test]
    fn global_persistence_failure_keeps_active_config_and_reports_warning() {
        let home = tempfile::tempdir().expect("temp home");
        let blocker = home.path().join("not-a-directory");
        std::fs::write(&blocker, "block").expect("write blocker");
        let _env = EnvGuard::nca_home(&blocker);
        let mut flow = configured_flow();
        assert!(matches!(
            reach_probe(&mut flow),
            OnboardingTransition::Probe(_)
        ));

        let OnboardingTransition::Completed(completion) =
            flow.apply_probe_outcome(OnboardingProbeOutcome::Succeeded)
        else {
            panic!("active setup should complete even when persistence fails");
        };
        assert!(completion.warning.is_some());
        assert!(completion.config.ui.onboarding_completed);
        assert_eq!(completion.config.provider.default, ProviderKind::Custom);
    }
}
