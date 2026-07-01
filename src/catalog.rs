use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;

use crate::config::{ModelKind, ModelOption, fallback_model_options, provider_connection};

const OPENROUTER_ASSISTANT_MODELS: &[&str] = &[
    "openai/gpt-5.5",
    "openai/gpt-5.4-mini",
    "anthropic/claude-sonnet-5",
    "anthropic/claude-opus-4.8",
    "google/gemini-3.5-flash",
    "x-ai/grok-4.3",
    "deepseek/deepseek-v4-pro",
    "qwen/qwen3.7-max",
    "moonshotai/kimi-k2.7-code",
    "mistralai/mistral-medium-3-5",
    "minimax/minimax-m3",
    "meta-llama/llama-4-maverick",
    "z-ai/glm-5.2",
    "z-ai/glm-5.1",
    "z-ai/glm-5-turbo",
    "z-ai/glm-5",
    "z-ai/glm-5v-turbo",
];

const OPENROUTER_IMAGE_MODELS: &[&str] = &[
    "openai/gpt-image-2",
    "google/gemini-3.1-flash-image",
    "x-ai/grok-imagine-image-quality",
    "recraft/recraft-v4.1",
    "black-forest-labs/flux.2-pro",
    "bytedance-seed/seedream-4.5",
];

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
    match discover_models(&client).await {
        Ok(discovered) if !discovered.is_empty() => models.extend(discovered),
        Ok(_) => {
            warnings.push("OpenRouter returned no compatible models".to_owned());
            models = fallback_model_options();
        }
        Err(error) => {
            warnings.push(format!("OpenRouter discovery failed: {error:#}"));
            models = fallback_model_options();
        }
    }
    models.sort_by(|left, right| model_sort_key(left).cmp(&model_sort_key(right)));
    models.dedup();
    ModelCatalog { models, warnings }
}

async fn discover_models(client: &Client) -> Result<Vec<ModelOption>> {
    let (api_key, base_url) = provider_connection()?;
    let url = format!(
        "{}/models?output_modalities=all",
        base_url.trim_end_matches('/')
    );
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .send()
        .await
        .context("cannot reach the OpenRouter models endpoint")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("cannot read model catalog response")?;
    if !status.is_success() {
        bail!("{status}: {}", body.chars().take(300).collect::<String>());
    }
    parse_models(&body)
}

fn parse_models(body: &str) -> Result<Vec<ModelOption>> {
    let response: Value =
        serde_json::from_str(body).context("model catalog returned invalid JSON")?;
    let data = response
        .get("data")
        .and_then(Value::as_array)
        .context("model catalog did not contain a data array")?;
    let models = data
        .iter()
        .filter_map(|model| {
            let id = model.get("id").and_then(Value::as_str)?;
            compatible_model(id, model).map(|kind| ModelOption {
                id: id.to_owned(),
                kind,
            })
        })
        .collect::<BTreeSet<_>>();
    Ok(models.into_iter().collect())
}

fn compatible_model(id: &str, model: &Value) -> Option<ModelKind> {
    let outputs = model
        .pointer("/architecture/output_modalities")
        .and_then(Value::as_array);
    let supports = |value: &str| {
        outputs.is_some_and(|items| items.iter().any(|item| item.as_str() == Some(value)))
    };
    if OPENROUTER_IMAGE_MODELS.contains(&id) && supports("image") {
        return Some(ModelKind::ImageGeneration);
    }
    let has_tools = model
        .get("supported_parameters")
        .and_then(Value::as_array)
        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("tools")));
    (OPENROUTER_ASSISTANT_MODELS.contains(&id) && supports("text") && has_tools)
        .then_some(ModelKind::Assistant)
}

fn model_sort_key(model: &ModelOption) -> (u8, usize, &str) {
    let order = match model.kind {
        ModelKind::Assistant => OPENROUTER_ASSISTANT_MODELS,
        ModelKind::ImageGeneration => OPENROUTER_IMAGE_MODELS,
    };
    (
        match model.kind {
            ModelKind::Assistant => 0,
            ModelKind::ImageGeneration => 1,
        },
        order
            .iter()
            .position(|id| *id == model.id)
            .unwrap_or(usize::MAX),
        &model.id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters_provider_catalogs() {
        let openrouter = parse_models(
            r#"{"data":[
                {"id":"z-ai/glm-5.2","architecture":{"output_modalities":["text"]},"supported_parameters":["tools"]},
                {"id":"openai/gpt-image-2","architecture":{"output_modalities":["image"]},"supported_parameters":[]},
                {"id":"openai/gpt-5.5","architecture":{"output_modalities":["text"]},"supported_parameters":["tools"]}
            ]}"#,
        )
        .unwrap();
        assert_eq!(openrouter.len(), 3);
        assert!(
            openrouter.iter().any(|model| {
                model.id == "openai/gpt-5.5" && model.kind == ModelKind::Assistant
            })
        );
        assert!(
            openrouter
                .iter()
                .any(|model| { model.id == "z-ai/glm-5.2" && model.kind == ModelKind::Assistant })
        );
        assert!(openrouter.iter().any(|model| {
            model.id == "openai/gpt-image-2" && model.kind == ModelKind::ImageGeneration
        }));
    }
}
