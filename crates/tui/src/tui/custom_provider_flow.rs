//! Pure state machine shared by onboarding and in-session custom-provider setup.
//!
//! Rendering, probing, activation, and persistence belong to the surface that
//! owns the flow. This module only owns the draft, validation transitions, and
//! probe outcomes so both surfaces apply the same rules.

use nca_common::config::{
    CustomCredentialSource, CustomProviderConfig, CustomProviderConfigError, CustomProviderSetup,
    ProviderCompatibility, normalize_custom_provider_base_url, validate_custom_api_key_env_name,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomProviderSetupStage {
    Compatibility,
    BaseUrl,
    Credentials,
    Model,
    Probing,
    ProbeFailed { message: String, retryable: bool },
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomProviderProbeOutcome {
    Succeeded,
    RetryableFailure(String),
    HardFailure(String),
}

#[derive(Debug, Clone)]
pub enum CustomProviderSetupTransition {
    Continue,
    ContinueWithMessage(String),
    Probe(CustomProviderConfig),
    Completed(CustomProviderConfig),
    Cancelled,
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomProviderSetupDraft {
    pub compatibility: ProviderCompatibility,
    pub base_url: String,
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub model: String,
}

impl CustomProviderSetupDraft {
    fn from_config(config: &CustomProviderConfig) -> Self {
        Self {
            compatibility: config.compatibility,
            base_url: config.base_url.clone(),
            api_key_env: config.api_key_env.clone(),
            // The existing secret is deliberately never copied into the draft.
            api_key: None,
            model: config.model.clone(),
        }
    }

    fn setup(&self) -> CustomProviderSetup {
        CustomProviderSetup {
            compatibility: self.compatibility,
            base_url: self.base_url.clone(),
            api_key_env: self.api_key_env.clone(),
            credential: self
                .api_key
                .clone()
                .map(CustomCredentialSource::Inline)
                .unwrap_or(CustomCredentialSource::Preserve),
            model: self.model.clone(),
        }
    }
}

pub struct CustomProviderSetupFlow {
    existing: CustomProviderConfig,
    draft: CustomProviderSetupDraft,
    stage: CustomProviderSetupStage,
    pending_probe: Option<CustomProviderConfig>,
}

impl CustomProviderSetupFlow {
    pub fn new(config: &CustomProviderConfig) -> Self {
        Self {
            existing: config.clone(),
            draft: CustomProviderSetupDraft::from_config(config),
            stage: CustomProviderSetupStage::Compatibility,
            pending_probe: None,
        }
    }

    pub fn stage(&self) -> CustomProviderSetupStage {
        self.stage.clone()
    }

    pub fn draft(&self) -> &CustomProviderSetupDraft {
        &self.draft
    }

    pub fn resume_at(&mut self, stage: CustomProviderSetupStage) {
        self.stage = stage;
        self.pending_probe = None;
    }

    pub fn choose_compatibility(
        &mut self,
        compatibility: ProviderCompatibility,
    ) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::Compatibility {
            return self.blocked("custom compatibility is not being selected");
        }
        self.draft.compatibility = compatibility;
        self.stage = CustomProviderSetupStage::BaseUrl;
        CustomProviderSetupTransition::Continue
    }

    pub fn submit_base_url(
        &mut self,
        base_url: impl Into<String>,
    ) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::BaseUrl {
            return self.blocked("custom endpoint is not being entered");
        }
        let base_url = base_url.into();
        let normalized = match normalize_custom_provider_base_url(&base_url) {
            Ok(url) => url,
            Err(error) => return self.blocked(error.to_string()),
        };
        self.draft.base_url = normalized;
        self.stage = CustomProviderSetupStage::Credentials;
        CustomProviderSetupTransition::Continue
    }

    pub fn submit_credentials(
        &mut self,
        api_key_env: impl Into<String>,
        api_key: impl Into<String>,
    ) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::Credentials {
            return self.blocked("custom credentials are not being entered");
        }
        let api_key_env = api_key_env.into();
        if let Err(error) = validate_custom_api_key_env_name(&api_key_env) {
            return self.blocked(error.to_string());
        }
        self.draft.api_key_env = api_key_env;
        let api_key = api_key.into();
        self.draft.api_key = (!api_key.trim().is_empty()).then_some(api_key);
        self.stage = CustomProviderSetupStage::Model;
        CustomProviderSetupTransition::Continue
    }

    pub fn update_api_key_env(
        &mut self,
        api_key_env: impl Into<String>,
    ) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::Credentials {
            return self.blocked("custom credentials are not being entered");
        }
        let api_key_env = api_key_env.into();
        if let Err(error) = validate_custom_api_key_env_name(&api_key_env) {
            return self.blocked(error.to_string());
        }
        self.draft.api_key_env = api_key_env;
        CustomProviderSetupTransition::Continue
    }

    pub fn submit_model(&mut self, model: impl Into<String>) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::Model {
            return self.blocked("custom model is not being entered");
        }
        let model = model.into();
        if model.trim().is_empty() {
            return self.blocked("custom provider model is required");
        }
        self.draft.model = model;
        let candidate = match self.candidate() {
            Ok(config) => config,
            Err(error) => return self.blocked(error.to_string()),
        };
        if candidate.resolve_api_key().is_none() {
            return self.blocked("custom provider API key is required");
        }
        self.pending_probe = Some(candidate.clone());
        self.stage = CustomProviderSetupStage::Probing;
        CustomProviderSetupTransition::Probe(candidate)
    }

    pub fn apply_probe_outcome(
        &mut self,
        outcome: CustomProviderProbeOutcome,
    ) -> CustomProviderSetupTransition {
        if self.stage != CustomProviderSetupStage::Probing {
            return self.blocked("the custom provider probe is no longer active");
        }
        match outcome {
            CustomProviderProbeOutcome::Succeeded => self.complete(),
            CustomProviderProbeOutcome::RetryableFailure(message) => {
                self.stage = CustomProviderSetupStage::ProbeFailed {
                    message: message.clone(),
                    retryable: true,
                };
                CustomProviderSetupTransition::ContinueWithMessage(message)
            }
            CustomProviderProbeOutcome::HardFailure(message) => {
                self.stage = CustomProviderSetupStage::ProbeFailed {
                    message: message.clone(),
                    retryable: false,
                };
                CustomProviderSetupTransition::Blocked(message)
            }
        }
    }

    pub fn retry_probe(&mut self) -> CustomProviderSetupTransition {
        if !matches!(
            &self.stage,
            CustomProviderSetupStage::ProbeFailed {
                retryable: true,
                ..
            }
        ) {
            return self.blocked("the custom probe cannot be retried");
        }
        let Some(candidate) = self.pending_probe.clone() else {
            return self.blocked("there is no custom provider probe to retry");
        };
        self.stage = CustomProviderSetupStage::Probing;
        CustomProviderSetupTransition::Probe(candidate)
    }

    pub fn save_anyway(&mut self) -> CustomProviderSetupTransition {
        if !matches!(
            &self.stage,
            CustomProviderSetupStage::ProbeFailed {
                retryable: true,
                ..
            }
        ) {
            return self.blocked("Save anyway is available only after a retryable probe failure");
        }
        self.complete()
    }

    pub fn cancel(&mut self) -> CustomProviderSetupTransition {
        self.stage = CustomProviderSetupStage::Cancelled;
        self.pending_probe = None;
        CustomProviderSetupTransition::Cancelled
    }

    pub fn candidate(&self) -> Result<CustomProviderConfig, CustomProviderConfigError> {
        let mut candidate = self.existing.clone();
        candidate.apply_setup(self.draft.setup())?;
        Ok(candidate)
    }

    fn complete(&mut self) -> CustomProviderSetupTransition {
        let Some(candidate) = self.pending_probe.clone() else {
            return self.blocked("custom provider setup is incomplete");
        };
        self.stage = CustomProviderSetupStage::Complete;
        CustomProviderSetupTransition::Completed(candidate)
    }

    fn blocked(&self, message: impl Into<String>) -> CustomProviderSetupTransition {
        CustomProviderSetupTransition::Blocked(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CustomProviderConfig {
        CustomProviderConfig {
            base_url: "https://old.example".into(),
            api_key_env: "OLD_API_KEY".into(),
            api_key: Some("old-secret".into()),
            model: "old-model".into(),
            ..Default::default()
        }
    }

    fn reach_probe(flow: &mut CustomProviderSetupFlow, secret: &str) {
        assert!(matches!(
            flow.choose_compatibility(ProviderCompatibility::Anthropic),
            CustomProviderSetupTransition::Continue
        ));
        assert!(matches!(
            flow.submit_base_url("https://gateway.example/v1"),
            CustomProviderSetupTransition::Continue
        ));
        assert!(matches!(
            flow.submit_credentials("GATEWAY_API_KEY", secret),
            CustomProviderSetupTransition::Continue
        ));
        assert!(matches!(
            flow.submit_model("gateway-model"),
            CustomProviderSetupTransition::Probe(_)
        ));
    }

    #[test]
    fn edit_prefills_public_fields_but_not_secret() {
        let flow = CustomProviderSetupFlow::new(&config());
        assert_eq!(flow.draft().base_url, "https://old.example");
        assert_eq!(flow.draft().api_key_env, "OLD_API_KEY");
        assert_eq!(flow.draft().model, "old-model");
        assert_eq!(flow.draft().api_key, None);
    }

    #[test]
    fn responses_compatibility_is_preserved_through_setup_candidate() {
        let mut flow = CustomProviderSetupFlow::new(&config());
        assert!(matches!(
            flow.choose_compatibility(ProviderCompatibility::OpenAiResponses),
            CustomProviderSetupTransition::Continue
        ));
        flow.submit_base_url("https://gateway.example/v1");
        flow.submit_credentials("GATEWAY_API_KEY", "secret");
        let CustomProviderSetupTransition::Probe(candidate) = flow.submit_model("responses-model")
        else {
            panic!("expected probe");
        };
        assert_eq!(
            candidate.compatibility,
            ProviderCompatibility::OpenAiResponses
        );
    }

    #[test]
    fn blank_secret_preserves_existing_credential() {
        let mut flow = CustomProviderSetupFlow::new(&config());
        flow.choose_compatibility(ProviderCompatibility::OpenAi);
        flow.submit_base_url("https://gateway.example");
        flow.submit_credentials("GATEWAY_API_KEY", "");
        let CustomProviderSetupTransition::Probe(candidate) = flow.submit_model("model") else {
            panic!("expected probe");
        };
        assert_eq!(candidate.api_key.as_deref(), Some("old-secret"));
        assert_eq!(candidate.api_key_env, "GATEWAY_API_KEY");
    }

    #[test]
    fn invalid_url_and_environment_name_block_before_probe() {
        let mut flow = CustomProviderSetupFlow::new(&config());
        flow.choose_compatibility(ProviderCompatibility::OpenAi);
        assert!(matches!(
            flow.submit_base_url("https://gateway.example/v1/chat/completions"),
            CustomProviderSetupTransition::Blocked(_)
        ));
        assert!(matches!(
            flow.submit_base_url("https://gateway.example"),
            CustomProviderSetupTransition::Continue
        ));
        assert!(matches!(
            flow.submit_credentials("bad-name", "secret"),
            CustomProviderSetupTransition::Blocked(_)
        ));
    }

    #[test]
    fn retryable_failure_supports_retry_save_anyway_and_cancel() {
        let mut flow = CustomProviderSetupFlow::new(&config());
        reach_probe(&mut flow, "new-secret");
        assert!(matches!(
            flow.apply_probe_outcome(CustomProviderProbeOutcome::RetryableFailure(
                "offline".into()
            )),
            CustomProviderSetupTransition::ContinueWithMessage(message) if message == "offline"
        ));
        assert!(matches!(
            flow.retry_probe(),
            CustomProviderSetupTransition::Probe(_)
        ));

        flow.apply_probe_outcome(CustomProviderProbeOutcome::RetryableFailure(
            "offline".into(),
        ));
        assert!(matches!(
            flow.save_anyway(),
            CustomProviderSetupTransition::Completed(_)
        ));

        let mut cancelled = CustomProviderSetupFlow::new(&config());
        reach_probe(&mut cancelled, "new-secret");
        cancelled.apply_probe_outcome(CustomProviderProbeOutcome::RetryableFailure(
            "offline".into(),
        ));
        assert!(matches!(
            cancelled.cancel(),
            CustomProviderSetupTransition::Cancelled
        ));
    }

    #[test]
    fn hard_failure_cannot_save_anyway() {
        let mut flow = CustomProviderSetupFlow::new(&config());
        reach_probe(&mut flow, "new-secret");
        assert!(matches!(
            flow.apply_probe_outcome(CustomProviderProbeOutcome::HardFailure(
                "bad protocol".into()
            )),
            CustomProviderSetupTransition::Blocked(message) if message == "bad protocol"
        ));
        assert!(matches!(
            flow.save_anyway(),
            CustomProviderSetupTransition::Blocked(_)
        ));
    }
}
