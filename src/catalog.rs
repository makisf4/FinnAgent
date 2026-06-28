use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::config::{ModelOption, Provider, fallback_model_options, provider_connection};

pub struct ModelCatalog {
    pub models: Vec<ModelOption>,
    pub warnings: Vec<String>,
}

pub async fn discover() -> ModelCatalog {
    let client = match Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return ModelCatalog {
                models: fallback_model_options(),
                warnings: vec![format!("cannot initialize model discovery: {error}")],
            };
        }
    };

    let mut models = Vec::new();
    let mut warnings = Vec::new();
    for provider in [Provider::OpenAi, Provider::OpenRouter] {
        match discover_provider(&client, provider).await {
            Ok(provider_models) if !provider_models.is_empty() => models.extend(provider_models),
            Ok(_) => {
                warnings.push(format!("{provider} returned no compatible models"));
                add_fallbacks(&mut models, provider);
            }
            Err(error) => {
                warnings.push(format!("{provider} discovery failed: {error:#}"));
                add_fallbacks(&mut models, provider);
            }
        }
    }
    models.sort_by(|left, right| {
        left.provider
            .to_string()
            .cmp(&right.provider.to_string())
            .then_with(|| right.id.cmp(&left.id))
    });
    models.dedup();
    ModelCatalog { models, warnings }
}

async fn discover_provider(client: &Client, provider: Provider) -> Result<Vec<ModelOption>> {
    let (api_key, base_url) = provider_connection(provider)?;
    let response = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .send()
        .await
        .with_context(|| format!("cannot reach the {provider} models endpoint"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("cannot read model catalog response")?;
    if !status.is_success() {
        bail!("{status}: {}", body.chars().take(300).collect::<String>());
    }
    parse_models(provider, &body)
}

fn parse_models(provider: Provider, body: &str) -> Result<Vec<ModelOption>> {
    let response: Value =
        serde_json::from_str(body).context("model catalog returned invalid JSON")?;
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .context("model catalog did not contain a data array")?;
    let ids = data
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .filter(|id| compatible_model(provider, id))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    Ok(ids
        .into_iter()
        .map(|id| ModelOption { provider, id })
        .collect())
}

fn compatible_model(provider: Provider, id: &str) -> bool {
    match provider {
        Provider::OpenAi => {
            id.starts_with("gpt-5")
                && !id.contains("image")
                && !id
                    .split('-')
                    .any(|part| part.len() == 4 && part.starts_with("202"))
        }
        Provider::OpenRouter => id.starts_with("z-ai/glm-"),
    }
}

fn add_fallbacks(models: &mut Vec<ModelOption>, provider: Provider) {
    models.extend(
        fallback_model_options()
            .into_iter()
            .filter(|model| model.provider == provider),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters_provider_catalogs() {
        let openai = parse_models(
            Provider::OpenAi,
            r#"{"data":[{"id":"gpt-5.5"},{"id":"gpt-image-1"},{"id":"text-embedding-3"}]}"#,
        )
        .unwrap();
        assert_eq!(openai.len(), 1);
        assert_eq!(openai[0].id, "gpt-5.5");

        let openrouter = parse_models(
            Provider::OpenRouter,
            r#"{"data":[{"id":"z-ai/glm-5.2"},{"id":"openai/gpt-5.5"}]}"#,
        )
        .unwrap();
        assert_eq!(openrouter.len(), 1);
        assert_eq!(openrouter[0].id, "z-ai/glm-5.2");
    }
}
