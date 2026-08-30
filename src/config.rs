use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub(crate) const DEFAULT_MODEL: &str = "moonshotai/kimi-k3";

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
        ("moonshotai/kimi-k3", ModelKind::Assistant),
        ("openai/gpt-5.6-terra", ModelKind::Assistant),
        ("openai/gpt-5.5", ModelKind::Assistant),
        ("deepseek/deepseek-v4-pro", ModelKind::Assistant),
        ("deepseek/deepseek-v4-flash", ModelKind::Assistant),
        ("z-ai/glm-5.3-flash", ModelKind::Assistant),
        ("openai/gpt-image-2", ModelKind::ImageGeneration),
        ("google/gemini-3.1-flash-image", ModelKind::ImageGeneration),
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
                env::var("FINN_VISION_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
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
        assert_eq!(
            models.first().map(|model| model.id.as_str()),
            Some(DEFAULT_MODEL)
        );
        assert!(models.contains(&ModelOption {
            id: "deepseek/deepseek-v4-pro".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "openai/gpt-5.5".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "openai/gpt-5.6-terra".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "z-ai/glm-5.3-flash".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "moonshotai/kimi-k3".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert!(models.contains(&ModelOption {
            id: "deepseek/deepseek-v4-flash".to_owned(),
            kind: ModelKind::Assistant,
        }));
        assert_eq!(
            model_kind_for_id("openai/gpt-image-2"),
            ModelKind::ImageGeneration
        );
        assert_eq!(
            model_kind_for_id("google/gemini-3.1-flash-image"),
            ModelKind::ImageGeneration
        );
        assert_eq!(models.len(), 8);
    }
}
