use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_MODEL: &str = "z-ai/glm-5.2";

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum ModelKind {
    #[default]
    Assistant,
    ImageGeneration,
}

impl ModelKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::ImageGeneration => "image generation",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModelOption {
    pub id: String,
    pub kind: ModelKind,
}

pub fn fallback_model_options() -> Vec<ModelOption> {
    [
        ("openai/gpt-5.5", ModelKind::Assistant),
        ("openai/gpt-5.4-mini", ModelKind::Assistant),
        ("anthropic/claude-sonnet-5", ModelKind::Assistant),
        ("anthropic/claude-opus-4.8", ModelKind::Assistant),
        ("google/gemini-3.5-flash", ModelKind::Assistant),
        ("x-ai/grok-4.3", ModelKind::Assistant),
        ("deepseek/deepseek-v4-pro", ModelKind::Assistant),
        ("qwen/qwen3.7-max", ModelKind::Assistant),
        ("moonshotai/kimi-k2.7-code", ModelKind::Assistant),
        ("mistralai/mistral-medium-3-5", ModelKind::Assistant),
        ("minimax/minimax-m3", ModelKind::Assistant),
        ("meta-llama/llama-4-maverick", ModelKind::Assistant),
        ("z-ai/glm-5.2", ModelKind::Assistant),
        ("z-ai/glm-5.1", ModelKind::Assistant),
        ("z-ai/glm-5-turbo", ModelKind::Assistant),
        ("z-ai/glm-5", ModelKind::Assistant),
        ("z-ai/glm-5v-turbo", ModelKind::Assistant),
        ("openai/gpt-image-2", ModelKind::ImageGeneration),
        ("google/gemini-3.1-flash-image", ModelKind::ImageGeneration),
        (
            "x-ai/grok-imagine-image-quality",
            ModelKind::ImageGeneration,
        ),
        ("recraft/recraft-v4.1", ModelKind::ImageGeneration),
        ("black-forest-labs/flux.2-pro", ModelKind::ImageGeneration),
        ("bytedance-seed/seedream-4.5", ModelKind::ImageGeneration),
    ]
    .into_iter()
    .map(|(id, kind)| ModelOption {
        id: id.to_owned(),
        kind,
    })
    .collect()
}

pub fn model_kind_for_id(id: &str) -> ModelKind {
    fallback_model_options()
        .into_iter()
        .find(|model| model.id == id)
        .map(|model| model.kind)
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct Config {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub model_kind: ModelKind,
    pub vision_model: Option<String>,
    pub reasoning_effort: String,
    pub home: PathBuf,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let (api_key, base_url) = provider_connection()?;
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set.")?;
        let data_dir = env::var_os("FINN_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                home.join("Library")
                    .join("Application Support")
                    .join("FinnAgent")
            });
        let model = env::var("FINN_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());

        Ok(Self {
            api_key,
            base_url,
            model_kind: model_kind_for_id(&model),
            model,
            vision_model: Some(
                env::var("FINN_VISION_MODEL").unwrap_or_else(|_| "z-ai/glm-5v-turbo".to_owned()),
            ),
            reasoning_effort: env::var("FINN_REASONING").unwrap_or_else(|_| "medium".to_owned()),
            home,
            data_dir,
        })
    }

    pub fn switched(&self, model: &ModelOption) -> Self {
        let mut config = self.clone();
        config.model = model.id.clone();
        config.model_kind = model.kind;
        config
    }
}

pub(crate) fn provider_connection() -> Result<(String, String)> {
    let api_key = env::var("OPENROUTER_API_KEY")
        .ok()
        .context("OPENROUTER_API_KEY is not set. Export it before running Finn.")?;
    if api_key.trim().is_empty() {
        bail!("OPENROUTER_API_KEY is empty.");
    }
    let base_url =
        env::var("OPENROUTER_BASE_URL").unwrap_or_else(|_| DEFAULT_OPENROUTER_BASE_URL.to_owned());
    Ok((api_key, base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_catalog_contains_assistant_and_image_models() {
        let models = fallback_model_options();
        assert!(models.contains(&ModelOption {
            id: "openai/gpt-5.5".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "z-ai/glm-5.2".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert_eq!(
            model_kind_for_id("openai/gpt-image-2"),
            ModelKind::ImageGeneration
        );
    }
}
