//! Public keyboard boundary for the in-session custom-provider setup flow.

use crate::tui::custom_provider_flow::CustomProviderSetupTransition;
use crate::tui::state::{CustomProviderSetupStep, TuiSessionState};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use nca_common::config::ProviderCompatibility;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProviderSetupSubmission {
    pub compatibility: ProviderCompatibility,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomProviderSetupKeyResult {
    Handled,
    ContinueWithPreservedCredential,
    ContinueWithInlineCredential,
    Submit(CustomProviderSetupSubmission),
    InvalidInput(String),
    ProbeAction(CustomProviderProbeAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomProviderProbeAction {
    Retry,
    SaveAnyway,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderActivationOutcome {
    pub active: bool,
    pub warning: Option<String>,
}

/// Translate activation and persistence results into the user-visible
/// contract: activation survives a later workspace-save failure.
pub fn provider_activation_outcome(
    activation: Result<(), String>,
    persistence: Result<(), String>,
) -> ProviderActivationOutcome {
    match activation {
        Ok(()) => ProviderActivationOutcome {
            active: true,
            warning: persistence
                .err()
                .map(|error| format!("workspace save failed: {error}")),
        },
        Err(_) => ProviderActivationOutcome {
            active: false,
            warning: None,
        },
    }
}

pub fn custom_provider_submission(state: &TuiSessionState) -> CustomProviderSetupSubmission {
    if let Some(flow) = state.custom_provider_setup_flow() {
        let draft = flow.draft();
        return CustomProviderSetupSubmission {
            compatibility: draft.compatibility,
            base_url: draft.base_url.trim().to_string(),
            api_key_env: draft.api_key_env.trim().to_string(),
            api_key: draft
                .api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string),
            model: draft.model.trim().to_string(),
        };
    }
    CustomProviderSetupSubmission {
        compatibility: if state.custom_setup_compat_index() == 0 {
            ProviderCompatibility::OpenAi
        } else {
            ProviderCompatibility::Anthropic
        },
        base_url: state.custom_setup_base_url().trim().to_string(),
        api_key_env: state.custom_setup_api_key_env().trim().to_string(),
        api_key: (!state.custom_setup_api_key().trim().is_empty())
            .then(|| state.custom_setup_api_key().trim().to_string()),
        model: state.custom_setup_input().trim().to_string(),
    }
}

pub fn custom_provider_probe_action(
    state: &TuiSessionState,
    index: usize,
) -> Option<CustomProviderProbeAction> {
    state.custom_provider_probe_error()?;
    if !state.custom_provider_probe_retryable() {
        return (index == 0).then_some(CustomProviderProbeAction::Cancel);
    }
    match index {
        0 => Some(CustomProviderProbeAction::Retry),
        1 => Some(CustomProviderProbeAction::SaveAnyway),
        2 => Some(CustomProviderProbeAction::Cancel),
        _ => None,
    }
}

pub fn handle_custom_provider_setup_key(
    state: &mut TuiSessionState,
    key: KeyEvent,
) -> CustomProviderSetupKeyResult {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => {
            if state.custom_provider_setup_step() == CustomProviderSetupStep::Probe {
                if let Some(flow) = state.custom_provider_setup_flow_mut() {
                    flow.cancel();
                }
                CustomProviderSetupKeyResult::ProbeAction(CustomProviderProbeAction::Cancel)
            } else {
                state.close_custom_provider_setup();
                CustomProviderSetupKeyResult::Handled
            }
        }
        (KeyCode::Up, _)
            if state.custom_provider_setup_step() == CustomProviderSetupStep::Compatibility =>
        {
            if let Some(index) = state.custom_setup_compat_index_mut() {
                *index = index.saturating_sub(1);
            }
            CustomProviderSetupKeyResult::Handled
        }
        (KeyCode::Down, _)
            if state.custom_provider_setup_step() == CustomProviderSetupStep::Compatibility =>
        {
            if let Some(index) = state.custom_setup_compat_index_mut() {
                *index = (*index + 1).min(1);
            }
            CustomProviderSetupKeyResult::Handled
        }
        (KeyCode::Up, _) | (KeyCode::Down, _)
            if state.custom_provider_setup_step() == CustomProviderSetupStep::Probe
                && state.custom_provider_probe_error().is_some() =>
        {
            let next = match key.code {
                KeyCode::Up => state.custom_provider_probe_index().saturating_sub(1),
                KeyCode::Down => (state.custom_provider_probe_index() + 1)
                    .min(state.custom_provider_probe_action_count().saturating_sub(1)),
                _ => state.custom_provider_probe_index(),
            };
            if let Some(index) = state.custom_provider_probe_index_mut() {
                *index = next;
            }
            CustomProviderSetupKeyResult::Handled
        }
        (KeyCode::Enter, _)
            if state.custom_provider_setup_step() == CustomProviderSetupStep::Probe =>
        {
            let Some(action) =
                custom_provider_probe_action(state, state.custom_provider_probe_index())
            else {
                return CustomProviderSetupKeyResult::Handled;
            };
            if let Some(flow) = state.custom_provider_setup_flow_mut() {
                match action {
                    CustomProviderProbeAction::Retry => {
                        if matches!(flow.retry_probe(), CustomProviderSetupTransition::Probe(_)) {
                            state.sync_custom_provider_setup_from_flow();
                        }
                    }
                    CustomProviderProbeAction::SaveAnyway => {
                        let _ = flow.save_anyway();
                    }
                    CustomProviderProbeAction::Cancel => {
                        flow.cancel();
                    }
                }
            }
            CustomProviderSetupKeyResult::ProbeAction(action)
        }
        (KeyCode::Enter, _) => match state.custom_provider_setup_step() {
            CustomProviderSetupStep::Compatibility => {
                let compatibility = if state.custom_setup_compat_index() == 0 {
                    ProviderCompatibility::OpenAi
                } else {
                    ProviderCompatibility::Anthropic
                };
                let transition = state
                    .custom_provider_setup_flow_mut()
                    .map(|flow| flow.choose_compatibility(compatibility));
                match transition {
                    Some(CustomProviderSetupTransition::Continue) => {
                        state.sync_custom_provider_setup_from_flow();
                        CustomProviderSetupKeyResult::Handled
                    }
                    Some(CustomProviderSetupTransition::Blocked(error)) => {
                        CustomProviderSetupKeyResult::InvalidInput(error)
                    }
                    _ => CustomProviderSetupKeyResult::InvalidInput(
                        "custom provider setup is not initialized".into(),
                    ),
                }
            }
            CustomProviderSetupStep::BaseUrl => {
                if state.custom_setup_input().trim().is_empty() {
                    return CustomProviderSetupKeyResult::InvalidInput(
                        "enter a custom provider base URL".into(),
                    );
                }
                let input = state.custom_setup_input().to_string();
                let transition = state
                    .custom_provider_setup_flow_mut()
                    .map(|flow| flow.submit_base_url(input));
                match transition {
                    Some(CustomProviderSetupTransition::Continue) => {
                        state.sync_custom_provider_setup_from_flow();
                        CustomProviderSetupKeyResult::Handled
                    }
                    Some(CustomProviderSetupTransition::Blocked(error)) => {
                        CustomProviderSetupKeyResult::InvalidInput(error)
                    }
                    _ => CustomProviderSetupKeyResult::InvalidInput(
                        "custom provider setup is not initialized".into(),
                    ),
                }
            }
            CustomProviderSetupStep::ApiKey => {
                if state.custom_setup_credential_env_focus() {
                    if state.custom_setup_input().trim().is_empty() {
                        return CustomProviderSetupKeyResult::InvalidInput(
                            "enter an API-key environment variable name".into(),
                        );
                    }
                    let env_name = state.custom_setup_input().trim().to_string();
                    let transition = state
                        .custom_provider_setup_flow_mut()
                        .map(|flow| flow.update_api_key_env(env_name));
                    match transition {
                        Some(CustomProviderSetupTransition::Continue) => {
                            state.set_custom_setup_credential_env_focus(false);
                            let secret = state.custom_setup_api_key().to_string();
                            if let Some(current) = state.custom_setup_input_mut() {
                                *current = secret;
                            }
                            CustomProviderSetupKeyResult::Handled
                        }
                        Some(CustomProviderSetupTransition::Blocked(error)) => {
                            CustomProviderSetupKeyResult::InvalidInput(error)
                        }
                        _ => CustomProviderSetupKeyResult::InvalidInput(
                            "custom provider setup is not initialized".into(),
                        ),
                    }
                } else {
                    let credential = state.custom_setup_input().trim().to_string();
                    let env_name = state.custom_setup_api_key_env().to_string();
                    let transition = state
                        .custom_provider_setup_flow_mut()
                        .map(|flow| flow.submit_credentials(env_name, credential.clone()));
                    match transition {
                        Some(CustomProviderSetupTransition::Continue) => {
                            if let Some(key) = state.custom_setup_api_key_mut() {
                                *key = credential.clone();
                            }
                            state.sync_custom_provider_setup_from_flow();
                            if credential.is_empty() {
                                CustomProviderSetupKeyResult::ContinueWithPreservedCredential
                            } else {
                                CustomProviderSetupKeyResult::ContinueWithInlineCredential
                            }
                        }
                        Some(CustomProviderSetupTransition::Blocked(error)) => {
                            CustomProviderSetupKeyResult::InvalidInput(error)
                        }
                        _ => CustomProviderSetupKeyResult::InvalidInput(
                            "custom provider setup is not initialized".into(),
                        ),
                    }
                }
            }
            CustomProviderSetupStep::Model => {
                let model = if state.custom_setup_input().trim().is_empty() {
                    state.custom_setup_model_hint().trim().to_string()
                } else {
                    state.custom_setup_input().trim().to_string()
                };
                let transition = state
                    .custom_provider_setup_flow_mut()
                    .map(|flow| flow.submit_model(model));
                match transition {
                    Some(CustomProviderSetupTransition::Probe(_)) => {
                        state.sync_custom_provider_setup_from_flow();
                        CustomProviderSetupKeyResult::Submit(custom_provider_submission(state))
                    }
                    Some(CustomProviderSetupTransition::Blocked(error)) => {
                        CustomProviderSetupKeyResult::InvalidInput(error)
                    }
                    _ => CustomProviderSetupKeyResult::InvalidInput(
                        "custom provider setup is not initialized".into(),
                    ),
                }
            }
            CustomProviderSetupStep::Probe => CustomProviderSetupKeyResult::Handled,
        },
        (KeyCode::Tab, _)
            if state.custom_provider_setup_step() == CustomProviderSetupStep::ApiKey =>
        {
            if state.custom_setup_credential_env_focus() {
                let env_name = state.custom_setup_input().trim().to_string();
                if !env_name.is_empty()
                    && let Some(api_key_env) = state.custom_setup_api_key_env_mut()
                {
                    *api_key_env = env_name;
                }
                let secret = state.custom_setup_api_key().to_string();
                if let Some(input) = state.custom_setup_input_mut() {
                    *input = secret;
                }
                state.set_custom_setup_credential_env_focus(false);
            } else {
                let secret = state.custom_setup_input().to_string();
                if let Some(api_key) = state.custom_setup_api_key_mut() {
                    *api_key = secret;
                }
                let env_name = state.custom_setup_api_key_env().to_string();
                if let Some(input) = state.custom_setup_input_mut() {
                    *input = env_name;
                }
                state.set_custom_setup_credential_env_focus(true);
            }
            CustomProviderSetupKeyResult::Handled
        }
        (KeyCode::Backspace, _)
            if state.custom_provider_setup_step() != CustomProviderSetupStep::Compatibility =>
        {
            if let Some(input) = state.custom_setup_input_mut() {
                input.pop();
            }
            CustomProviderSetupKeyResult::Handled
        }
        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT)
            if state.custom_provider_setup_step() != CustomProviderSetupStep::Compatibility
                && state.custom_provider_setup_step() != CustomProviderSetupStep::Probe =>
        {
            if let Some(input) = state.custom_setup_input_mut() {
                input.push(c);
            }
            CustomProviderSetupKeyResult::Handled
        }
        _ => CustomProviderSetupKeyResult::Handled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::TuiSessionState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use nca_common::config::{CustomProviderConfig, ProviderCompatibility};
    use std::path::PathBuf;

    fn state() -> TuiSessionState {
        TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
        )
    }

    #[test]
    fn credential_step_accepts_blank_secret_as_source_preserving_submission() {
        let mut st = state();
        let mut config = CustomProviderConfig::default();
        config.compatibility = ProviderCompatibility::OpenAi;
        config.base_url = "https://gateway.example".into();
        config.api_key_env = "GATEWAY_API_KEY".into();
        config.model = "gateway-model".into();
        st.open_custom_provider_setup_from_config(&config);

        while st.custom_provider_setup_step() != CustomProviderSetupStep::ApiKey {
            handle_custom_provider_setup_key(
                &mut st,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }
        let result = handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(result, CustomProviderSetupKeyResult::Handled);
        let result = handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(
            result,
            CustomProviderSetupKeyResult::ContinueWithPreservedCredential
        ));
    }

    #[test]
    fn setup_submission_contains_env_name_without_secret_when_editing() {
        let mut st = state();
        let mut config = CustomProviderConfig::default();
        config.base_url = "https://gateway.example".into();
        config.api_key_env = "GATEWAY_API_KEY".into();
        config.api_key = Some("secret-not-for-ui".into());
        config.model = "gateway-model".into();
        st.open_custom_provider_setup_from_config(&config);

        let submission = custom_provider_submission(&st);
        assert_eq!(submission.api_key_env, "GATEWAY_API_KEY");
        assert!(submission.api_key.is_none());
    }

    #[test]
    fn tab_toggles_between_environment_name_and_secret_without_leaking_existing_secret() {
        let mut st = state();
        let config = CustomProviderConfig {
            base_url: "https://gateway.example".into(),
            api_key_env: "GATEWAY_API_KEY".into(),
            api_key: Some("stored-secret".into()),
            model: "gateway-model".into(),
            ..Default::default()
        };
        st.open_custom_provider_setup_from_config(&config);
        handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(st.custom_setup_credential_env_focus());
        assert_eq!(st.custom_setup_input(), "GATEWAY_API_KEY");
        handle_custom_provider_setup_key(&mut st, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!st.custom_setup_credential_env_focus());
        assert!(st.custom_setup_input().is_empty());

        handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        handle_custom_provider_setup_key(&mut st, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(st.custom_setup_credential_env_focus());
        assert_eq!(st.custom_setup_input(), "GATEWAY_API_KEY");
        assert_eq!(st.custom_setup_api_key(), "x");
    }

    #[test]
    fn failed_probe_offers_retry_save_anyway_and_cancel() {
        let mut st = state();
        st.open_custom_provider_setup_from_config(&CustomProviderConfig {
            base_url: "https://gateway.example".into(),
            api_key: Some("secret".into()),
            model: "gateway-model".into(),
            ..Default::default()
        });
        st.custom_provider_setup_flow_mut()
            .expect("setup flow")
            .resume_at(crate::tui::custom_provider_flow::CustomProviderSetupStage::Probing);
        st.start_custom_provider_probe();
        let transition = st.apply_custom_provider_probe_outcome(
            crate::tui::custom_provider_flow::CustomProviderProbeOutcome::RetryableFailure(
                "connection failed".into(),
            ),
        );
        assert!(matches!(
            transition,
            crate::tui::custom_provider_flow::CustomProviderSetupTransition::ContinueWithMessage(
                message
            ) if message == "connection failed"
        ));

        assert_eq!(st.custom_provider_probe_error(), Some("connection failed"));
        assert_eq!(st.custom_provider_probe_action_count(), 3);
        assert_eq!(
            custom_provider_probe_action(&st, 0),
            Some(CustomProviderProbeAction::Retry)
        );
        assert_eq!(
            custom_provider_probe_action(&st, 1),
            Some(CustomProviderProbeAction::SaveAnyway)
        );
        assert_eq!(
            custom_provider_probe_action(&st, 2),
            Some(CustomProviderProbeAction::Cancel)
        );
    }

    #[test]
    fn hard_probe_failure_only_offers_cancel() {
        let mut st = state();
        st.open_custom_provider_setup_from_config(&CustomProviderConfig {
            base_url: "https://gateway.example".into(),
            api_key: Some("secret".into()),
            model: "gateway-model".into(),
            ..Default::default()
        });
        st.custom_provider_setup_flow_mut()
            .expect("setup flow")
            .resume_at(crate::tui::custom_provider_flow::CustomProviderSetupStage::Probing);
        st.start_custom_provider_probe();
        let _ = st.apply_custom_provider_probe_outcome(
            crate::tui::custom_provider_flow::CustomProviderProbeOutcome::HardFailure(
                "invalid protocol".into(),
            ),
        );

        assert_eq!(st.custom_provider_probe_action_count(), 1);
        assert_eq!(
            custom_provider_probe_action(&st, 0),
            Some(CustomProviderProbeAction::Cancel)
        );
        assert_eq!(custom_provider_probe_action(&st, 1), None);
    }

    #[test]
    fn keyboard_transitions_drive_the_shared_setup_flow_to_probe() {
        let mut st = state();
        let config = CustomProviderConfig {
            base_url: "https://gateway.example".into(),
            api_key_env: "GATEWAY_API_KEY".into(),
            api_key: Some("existing-secret".into()),
            model: "gateway-model".into(),
            ..Default::default()
        };
        st.open_custom_provider_setup_from_config(&config);

        for _ in 0..2 {
            handle_custom_provider_setup_key(
                &mut st,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            );
        }
        handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        let result = handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert!(matches!(
            result,
            CustomProviderSetupKeyResult::ContinueWithPreservedCredential
        ));
        let result = handle_custom_provider_setup_key(
            &mut st,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(matches!(
            result,
            CustomProviderSetupKeyResult::Submit(CustomProviderSetupSubmission {
                api_key: None,
                model,
                ..
            }) if model == "gateway-model"
        ));
        assert!(matches!(
            st.custom_provider_setup_flow().map(|flow| flow.stage()),
            Some(crate::tui::custom_provider_flow::CustomProviderSetupStage::Probing)
        ));
    }

    #[test]
    fn persistence_failure_keeps_activation_and_surfaces_warning() {
        let outcome = provider_activation_outcome(Ok(()), Err("disk full".into()));
        assert!(outcome.active);
        assert_eq!(
            outcome.warning.as_deref(),
            Some("workspace save failed: disk full")
        );
    }

    #[test]
    fn activation_failure_does_not_report_a_persisted_active_provider() {
        let outcome = provider_activation_outcome(Err("invalid credentials".into()), Ok(()));
        assert!(!outcome.active);
        assert!(outcome.warning.is_none());
    }
}
