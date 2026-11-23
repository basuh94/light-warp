use light_llm::ModelInfo;

use super::{LocalModelConfig, llm_id_for_model, model_name_for_llm_id, models_by_feature};
use crate::ai::llms::LLMId;

fn installed(names: &[&str]) -> Vec<ModelInfo> {
    names
        .iter()
        .map(|name| ModelInfo {
            name: (*name).to_owned(),
            size: 0,
        })
        .collect()
}

fn config(default_model: &str) -> LocalModelConfig {
    LocalModelConfig {
        base_url: "http://127.0.0.1:11434".to_owned(),
        default_model: default_model.to_owned(),
        context_window: 16_384,
    }
}

#[test]
fn llm_ids_round_trip_model_names_with_colons() {
    let id = llm_id_for_model("qwen3:8b");

    assert_eq!(id.as_str(), "ollama:qwen3:8b");
    assert_eq!(model_name_for_llm_id(&id), Some("qwen3:8b"));
}

#[test]
fn ids_of_other_models_have_no_local_name() {
    assert_eq!(model_name_for_llm_id(&LLMId::from("auto")), None);
    assert_eq!(model_name_for_llm_id(&LLMId::from("ollama:")), None);
}

#[test]
fn catalog_offers_every_installed_model_and_prefers_the_configured_default() {
    let models = models_by_feature(&installed(&["qwen3:4b", "qwen3:8b"]), &config("qwen3:8b"))
        .expect("installed models make a catalog");

    let names: Vec<_> = models
        .agent_mode
        .choices_for_test()
        .iter()
        .map(|info| info.display_name.as_str())
        .collect();
    assert_eq!(names, vec!["qwen3:4b", "qwen3:8b"]);
    assert_eq!(
        models.agent_mode.default_id_for_test(),
        &llm_id_for_model("qwen3:8b")
    );
}

#[test]
fn catalog_falls_back_to_the_first_model_when_the_default_is_missing() {
    let models = models_by_feature(&installed(&["llama3.2:3b"]), &config("qwen3:8b"))
        .expect("installed models make a catalog");

    assert_eq!(
        models.agent_mode.default_id_for_test(),
        &llm_id_for_model("llama3.2:3b")
    );
}

#[test]
fn no_installed_models_means_no_catalog() {
    assert!(models_by_feature(&[], &config("qwen3:8b")).is_none());
}
