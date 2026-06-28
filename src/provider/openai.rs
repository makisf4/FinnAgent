use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{ModelTurn, ToolCall, Usage, api_error_message, send_with_retry};
use crate::config::Config;
use crate::tools;

#[derive(Clone)]
pub struct OpenAi {
    api_key: String,
    base_url: String,
    model: String,
    reasoning_effort: String,
    history: Vec<Value>,
}

impl OpenAi {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            model: config.model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            history: Vec::new(),
        }
    }

    pub fn push_user(&mut self, task: &str) {
        self.history.push(json!({"role": "user", "content": task}));
    }

    pub fn push_user_image(&mut self, prompt: &str, data_url: &str) {
        self.history.push(image_message(prompt, data_url));
    }

    pub fn push_assistant(&mut self, answer: &str) {
        self.history
            .push(json!({"role": "assistant", "content": answer}));
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str) {
        self.history.push(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": result,
        }));
    }

    pub async fn create_turn(&mut self, client: &reqwest::Client) -> Result<ModelTurn> {
        let instructions = crate::agent::instructions(&self.model, &self.reasoning_effort);
        let request = json!({
            "model": self.model,
            "reasoning": {"effort": self.reasoning_effort},
            "instructions": instructions,
            "input": self.history,
            "tools": tools::definitions(),
            "tool_choice": "auto",
            "parallel_tool_calls": false,
        });
        let request = client
            .post(format!("{}/responses", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request);
        let response = send_with_retry(request, "the OpenAI Responses API").await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("cannot read the OpenAI response")?;
        if !status.is_success() {
            bail!("OpenAI API returned {status}: {}", api_error_message(&body));
        }
        let response: Value =
            serde_json::from_str(&body).context("OpenAI returned invalid JSON")?;
        parse_response(&response, &mut self.history)
    }
}

fn image_message(prompt: &str, data_url: &str) -> Value {
    json!({
        "role": "user",
        "content": [
            {"type": "input_text", "text": prompt},
            {"type": "input_image", "image_url": data_url, "detail": "auto"}
        ]
    })
}

fn parse_response(response: &Value, history: &mut Vec<Value>) -> Result<ModelTurn> {
    let output = response
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .context("OpenAI response did not contain an output array")?;
    history.extend(output.iter().cloned());

    let mut tool_calls = Vec::new();
    for item in &output {
        if item.get("type").and_then(Value::as_str) != Some("function_call") {
            continue;
        }
        tool_calls.push(ToolCall {
            name: item
                .get("name")
                .and_then(Value::as_str)
                .context("function call missing name")?
                .to_owned(),
            id: item
                .get("call_id")
                .and_then(Value::as_str)
                .context("function call missing call_id")?
                .to_owned(),
            arguments: item
                .get("arguments")
                .and_then(Value::as_str)
                .context("function call missing arguments")?
                .to_owned(),
        });
    }

    Ok(ModelTurn {
        response_id: response
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        usage: parse_usage(response),
        answer: extract_output_text(&output),
        tool_calls,
    })
}

fn parse_usage(response: &Value) -> Usage {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    Usage {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cached_input_tokens: usage
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        reasoning_tokens: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn extract_output_text(output: &[Value]) -> Option<String> {
    let mut parts = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in content {
            if part.get("type").and_then(Value::as_str) == Some("output_text")
                && let Some(text) = part.get("text").and_then(Value::as_str)
            {
                parts.push(text);
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Provider;
    use crate::provider::test_support;

    #[test]
    fn parses_response_usage_and_text() {
        let response = json!({
            "id": "resp_1",
            "output": [{"type": "message", "content": [
                {"type": "output_text", "text": "Done."}
            ]}],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 3,
                "total_tokens": 15
            }
        });
        let turn = parse_response(&response, &mut Vec::new()).unwrap();
        assert_eq!(turn.answer.as_deref(), Some("Done."));
        assert_eq!(turn.usage.total_tokens, 15);
    }

    #[test]
    fn serializes_responses_image_input() {
        let message = image_message("What is this?", "data:image/png;base64,YQ==");
        assert_eq!(message["content"][0]["type"], "input_text");
        assert_eq!(message["content"][1]["type"], "input_image");
        assert_eq!(
            message["content"][1]["image_url"],
            "data:image/png;base64,YQ=="
        );
    }

    #[tokio::test]
    async fn calls_mock_responses_endpoint() {
        let body = r#"{
            "id":"resp_mock",
            "output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}],
            "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
        }"#;
        let (base_url, server) = test_support::mock_http_server(vec![("200 OK", body)]).await;
        let config = Config {
            provider: Provider::OpenAi,
            api_key: "test-key".to_owned(),
            base_url,
            model: "gpt-test".to_owned(),
            vision_model: None,
            reasoning_effort: "medium".to_owned(),
            home: PathBuf::from("/tmp"),
            data_dir: PathBuf::from("/tmp"),
        };
        let mut provider = OpenAi::new(&config);
        provider.push_user("hello");
        let turn = provider.create_turn(&reqwest::Client::new()).await.unwrap();
        assert_eq!(turn.answer.as_deref(), Some("ok"));
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /responses "));
        assert!(requests[0].contains("authorization: Bearer test-key"));
    }
}
