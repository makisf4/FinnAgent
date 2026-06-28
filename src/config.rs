use std::env;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result, bail};

const DEFAULT_OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOption {
    pub provider: Provider,
    pub id: String,
}

pub fn fallback_model_options() -> Vec<ModelOption> {
    [
        (Provider::OpenAi, "gpt-5.5"),
        (Provider::OpenAi, "gpt-5.4"),
        (Provider::OpenAi, "gpt-5.4-mini"),
        (Provider::OpenAi, "gpt-5.4-nano"),
        (Provider::OpenRouter, "z-ai/glm-5.2"),
        (Provider::OpenRouter, "z-ai/glm-5.1"),
        (Provider::OpenRouter, "z-ai/glm-5-turbo"),
        (Provider::OpenRouter, "z-ai/glm-5"),
        (Provider::OpenRouter, "z-ai/glm-5v-turbo"),
        (Provider::OpenRouter, "z-ai/glm-4.7"),
    ]
    .into_iter()
    .map(|(provider, id)| ModelOption {
        provider,
        id: id.to_owned(),
    })
    .collect()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Provider {
    #[default]
    OpenAi,
    OpenRouter,
}

impl Provider {
    pub fn default_model(self) -> &'static str {
        match self {
            Self::OpenAi => "gpt-5.5",
            Self::OpenRouter => "z-ai/glm-5.2",
        }
    }

    pub fn api_label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI Responses",
            Self::OpenRouter => "OpenRouter Chat Completions",
        }
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenAi => formatter.write_str("openai"),
            Self::OpenRouter => formatter.write_str("openrouter"),
        }
    }
}

impl FromStr for Provider {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "openai" => Ok(Self::OpenAi),
            "openrouter" => Ok(Self::OpenRouter),
            other => {
                bail!("unsupported FINN_PROVIDER '{other}'; expected 'openai' or 'openrouter'")
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub provider: Provider,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub vision_model: Option<String>,
    pub reasoning_effort: String,
    pub home: PathBuf,
    pub data_dir: PathBuf,
}

impl Config {
    pub fn load() -> Result<Self> {
        let provider = env::var("FINN_PROVIDER")
            .unwrap_or_else(|_| "openai".to_owned())
            .parse()?;
        let (api_key, base_url) = provider_connection(provider)?;

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

        Ok(Self {
            provider,
            api_key,
            base_url,
            model: env::var("FINN_MODEL").unwrap_or_else(|_| provider.default_model().to_owned()),
            vision_model: vision_model(provider),
            reasoning_effort: env::var("FINN_REASONING").unwrap_or_else(|_| "medium".to_owned()),
            home,
            data_dir,
        })
    }

    pub fn switched(&self, provider: Provider, model: &str) -> Result<Self> {
        let (api_key, base_url) = provider_connection(provider)?;
        let mut config = self.clone();
        config.provider = provider;
        config.api_key = api_key;
        config.base_url = base_url;
        config.model = model.to_owned();
        config.vision_model = vision_model(provider);
        Ok(config)
    }
}

fn vision_model(provider: Provider) -> Option<String> {
    env::var("FINN_VISION_MODEL")
        .ok()
        .or_else(|| (provider == Provider::OpenRouter).then(|| "z-ai/glm-5v-turbo".to_owned()))
}

pub(crate) fn provider_connection(provider: Provider) -> Result<(String, String)> {
    let (key_name, api_key, base_url) = match provider {
        Provider::OpenAi => (
            "OPENAI_API_KEY",
            env::var("OPENAI_API_KEY").ok(),
            "https://api.openai.com/v1".to_owned(),
        ),
        Provider::OpenRouter => (
            "OPENROUTER_API_KEY",
            env::var("OPENROUTER_API_KEY").ok(),
            env::var("OPENROUTER_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_OPENROUTER_BASE_URL.to_owned()),
        ),
    };
    let api_key = api_key.with_context(|| {
        format!("{key_name} is not set. Export it before selecting this provider.")
    })?;
    if api_key.trim().is_empty() {
        bail!("{key_name} is empty.");
    }
    Ok((api_key, base_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_providers_case_insensitively() {
        assert_eq!("openai".parse::<Provider>().unwrap(), Provider::OpenAi);
        assert_eq!(
            "OpenRouter".parse::<Provider>().unwrap(),
            Provider::OpenRouter
        );
    }

    #[test]
    fn rejects_unsupported_provider() {
        let error = "ollama".parse::<Provider>().unwrap_err().to_string();
        assert!(error.contains("unsupported FINN_PROVIDER"));
    }

    #[test]
    fn provider_defaults_are_distinct() {
        assert_eq!(Provider::default(), Provider::OpenAi);
        assert_eq!(Provider::OpenAi.default_model(), "gpt-5.5");
        assert_eq!(Provider::OpenRouter.default_model(), "z-ai/glm-5.2");
        let models = fallback_model_options();
        assert!(models.contains(&ModelOption {
            provider: Provider::OpenAi,
            id: "gpt-5.5".to_owned()
        }));
        assert!(models.contains(&ModelOption {
            provider: Provider::OpenRouter,
            id: "z-ai/glm-5.2".to_owned()
        }));
    }
}
