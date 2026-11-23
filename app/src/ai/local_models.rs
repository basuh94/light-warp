//! Models served by a locally hosted model server, used when the build reaches no Warp servers.
//!
//! The server is opt-in: nothing contacts it until the user enables it in the settings.

use std::collections::HashMap;

use warp_core::channel::ChannelState;
use warpui::{AppContext, SingletonEntity as _};

use crate::ai::llms::{
    AvailableLLMs, LLMContextWindow, LLMId, LLMInfo, LLMProvider, LLMUsageMetadata, ModelsByFeature,
};
use crate::settings::AISettings;

/// Prefix of the [`LLMId`]s of local models, followed by the model's name on the server.
const LOCAL_MODEL_ID_PREFIX: &str = "ollama:";

/// Fits a 7-8B model and its context in 8 GB of VRAM.
const DEFAULT_CONTEXT_WINDOW: u32 = 16_384;

/// Whether this build serves agent requests and the model list from a local model server
/// instead of Warp's servers.
pub fn is_enabled() -> bool {
    ChannelState::is_offline()
}

/// Where to find the local model server and how to run its models.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalModelConfig {
    pub base_url: String,
    /// The model to use when the request does not name an installed one. May be empty, in which
    /// case the first installed model is used.
    pub default_model: String,
    pub context_window: u32,
}

impl LocalModelConfig {
    /// The server the user configured, or `None` if they have not enabled one.
    pub fn from_settings(app: &AppContext) -> Option<Self> {
        let settings = AISettings::as_ref(app);
        if !*settings.local_model_enabled {
            return None;
        }
        let base_url = settings.local_model_url.trim();
        if base_url.is_empty() {
            return None;
        }
        Some(Self {
            base_url: base_url.to_owned(),
            default_model: settings.local_model_name.trim().to_owned(),
            context_window: u32::try_from(*settings.local_model_context_window)
                .ok()
                .filter(|window| *window > 0)
                .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        })
    }

    pub fn client(&self) -> light_llm::OllamaClient {
        light_llm::OllamaClient::new(self.base_url.clone())
    }
}

pub fn llm_id_for_model(model_name: &str) -> LLMId {
    format!("{LOCAL_MODEL_ID_PREFIX}{model_name}").into()
}

/// The server-side name of the model a local [`LLMId`] refers to.
pub fn model_name_for_llm_id(id: &LLMId) -> Option<&str> {
    id.as_str()
        .strip_prefix(LOCAL_MODEL_ID_PREFIX)
        .filter(|name| !name.is_empty())
}

/// Builds the model catalog from the models installed on the local server. Returns `None` when no
/// model is installed, since the catalog needs at least one choice.
pub fn models_by_feature(
    installed: &[light_llm::ModelInfo],
    config: &LocalModelConfig,
) -> Option<ModelsByFeature> {
    let choices: Vec<LLMInfo> = installed
        .iter()
        .map(|model| llm_info(&model.name))
        .collect();
    let default_name = installed
        .iter()
        .find(|model| model.name == config.default_model)
        .or_else(|| installed.first())?
        .name
        .clone();
    let available =
        || AvailableLLMs::new(llm_id_for_model(&default_name), choices.clone(), None).ok();
    Some(ModelsByFeature {
        agent_mode: available()?,
        coding: available()?,
        cli_agent: available(),
        computer_use: ModelsByFeature::default().computer_use,
    })
}

fn llm_info(model_name: &str) -> LLMInfo {
    LLMInfo {
        display_name: model_name.to_owned(),
        base_model_name: model_name.to_owned(),
        id: llm_id_for_model(model_name),
        reasoning_level: None,
        usage_metadata: LLMUsageMetadata {
            request_multiplier: 1,
            credit_multiplier: None,
        },
        description: Some("local".to_owned()),
        disable_reason: None,
        vision_supported: false,
        spec: None,
        provider: LLMProvider::Unknown,
        host_configs: HashMap::new(),
        discount_percentage: None,
        context_window: LLMContextWindow::default(),
    }
}

#[cfg(test)]
#[path = "local_models_tests.rs"]
mod tests;
