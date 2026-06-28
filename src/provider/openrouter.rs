use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::{ModelTurn, ToolCall, Usage, api_error_message, send_with_retry};
use crate::config::Config;
use crate::tools;

#[derive(Clone)]
pub struct OpenRouter {
    api_key: String,
    base_url: String,
    model: String,
    reasoning_effort: String,
    messages: Vec<Value>,
}

impl OpenRouter {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            model: config.model.clone(),
            reasoning_effort: config.reasoning_effort.clone(),
            messages: Vec::new(),
        }
    }

    pub fn push_user(&mut self, task: &str) {
        self.messages.push(json!({"role": "user", "content": task}));
    }

    pub fn push_user_image(&mut self, prompt: &str, data_url: &str) {
        self.messages.push(image_message(prompt, data_url));
    }

    pub fn push_assistant(&mut self, answer: &str) {
        self.messages
            .push(json!({"role": "assistant", "content": answer}));
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str) {
        self.messages.push(json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": result,
        }));
    }

    pub async fn create_turn(&mut self, client: &reqwest::Client) -> Result<ModelTurn> {
        let request = build_request(
            &self.model,
            &crate::agent::instructions(&self.model, &self.reasoning_effort),
            &self.messages,
        );
        let request = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request);
        let response = send_with_retry(request, "the OpenRouter Chat Completions API").await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("cannot read the OpenRouter response")?;
        if !status.is_success() {
            bail!(
                "OpenRouter API returned {status}: {}",
                api_error_message(&body)
            );
        }
        let response: Value =
            serde_json::from_str(&body).context("OpenRouter returned invalid JSON")?;
        let turn = parse_response(&response)?;
        let assistant = response
            .pointer("/choices/0/message")
            .cloned()
            .context("OpenRouter response did not contain choices[0].message")?;
        self.messages.push(assistant);
        Ok(turn)
    }
}

fn image_message(prompt: &str, data_url: &str) -> Value {
    json!({
        "role": "user",
        "content": [
            {"type": "text", "text": prompt},
            {"type": "image_url", "image_url": {"url": data_url}}
        ]
    })
}

fn build_request(model: &str, instructions: &str, history: &[Value]) -> Value {
    let mut messages = Vec::with_capacity(history.len() + 1);
    messages.push(json!({"role": "system", "content": instructions}));
    messages.extend_from_slice(history);
    json!({
        "model": model,
        "messages": messages,
        "tools": chat_tool_definitions(),
        "tool_choice": "auto",
        "parallel_tool_calls": false,
    })
}

fn chat_tool_definitions() -> Vec<Value> {
    tools::definitions()
        .into_iter()
        .map(|tool| {
            let object = tool.as_object().expect("tool definitions are objects");
            let mut function = Map::new();
            for key in ["name", "description", "strict", "parameters"] {
                if let Some(value) = object.get(key) {
                    function.insert(key.to_owned(), value.clone());
                }
            }
            json!({"type": "function", "function": function})
        })
        .collect()
}

fn parse_response(response: &Value) -> Result<ModelTurn> {
    let message = response
        .pointer("/choices/0/message")
        .and_then(Value::as_object)
        .context("OpenRouter response did not contain choices[0].message")?;
    let tool_calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) => calls
            .iter()
            .map(parse_tool_call)
            .collect::<Result<Vec<_>>>()?,
        Some(_) => bail!("OpenRouter tool-call format mismatch: tool_calls must be an array"),
    };
    let answer = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let usage = response.get("usage").unwrap_or(&Value::Null);

    Ok(ModelTurn {
        response_id: response
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        usage: Usage {
            input_tokens: usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            cached_input_tokens: usage
                .pointer("/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output_tokens: usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            reasoning_tokens: usage
                .pointer("/completion_tokens_details/reasoning_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            total_tokens: usage
                .get("total_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
        tool_calls,
        answer,
    })
}

fn parse_tool_call(call: &Value) -> Result<ToolCall> {
    if call.get("type").and_then(Value::as_str) != Some("function") {
        bail!("OpenRouter tool-call format mismatch: expected type 'function'");
    }
    let field = |pointer: &str, name: &str| {
        call.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .with_context(|| format!("OpenRouter tool-call format mismatch: missing {name}"))
    };
    Ok(ToolCall {
        id: field("/id", "id")?,
        name: field("/function/name", "function.name")?,
        arguments: field("/function/arguments", "function.arguments")?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::Provider;
    use crate::provider::test_support;

    #[test]
    fn serializes_chat_completions_request() {
        let request = build_request(
            "z-ai/glm-5.2",
            "System instructions",
            &[json!({"role": "user", "content": "List files"})],
        );
        assert_eq!(request["model"], "z-ai/glm-5.2");
        assert_eq!(request["messages"][0]["role"], "system");
        assert_eq!(request["messages"][1]["role"], "user");
        assert_eq!(request["tools"][0]["type"], "function");
        assert!(request["tools"][0].get("name").is_none());
        assert_eq!(request["tools"][0]["function"]["name"], "path_status");
    }

    #[test]
    fn serializes_chat_completions_image_input() {
        let message = image_message("What is this?", "data:image/png;base64,YQ==");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][1]["type"], "image_url");
        assert_eq!(
            message["content"][1]["image_url"]["url"],
            "data:image/png;base64,YQ=="
        );
    }

    #[test]
    fn parses_tool_call_response() {
        let response = json!({
            "id": "gen-1",
            "choices": [{"message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "path_status",
                        "arguments": "{\"path\":\"~/Desktop\"}"
                    }
                }]
            }}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let turn = parse_response(&response).unwrap();
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "call_1");
        assert_eq!(turn.tool_calls[0].name, "path_status");
        assert_eq!(turn.usage.total_tokens, 15);
    }

    #[test]
    fn rejects_malformed_tool_call() {
        let response = json!({
            "choices": [{"message": {
                "role": "assistant",
                "tool_calls": [{"id": "call_1", "type": "tool"}]
            }}]
        });
        let error = parse_response(&response).unwrap_err().to_string();
        assert!(error.contains("OpenRouter tool-call format mismatch"));
    }

    #[tokio::test]
    async fn calls_mock_chat_completions_endpoint() {
        let body = r#"{
            "id":"gen_mock",
            "choices":[{"message":{"role":"assistant","content":"ok"}}],
            "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
        }"#;
        let (base_url, server) = test_support::mock_http_server(vec![("200 OK", body)]).await;
        let config = Config {
            provider: Provider::OpenRouter,
            api_key: "test-key".to_owned(),
            base_url,
            model: "router/test".to_owned(),
            vision_model: None,
            reasoning_effort: "medium".to_owned(),
            home: PathBuf::from("/tmp"),
            data_dir: PathBuf::from("/tmp"),
        };
        let mut provider = OpenRouter::new(&config);
        provider.push_user("hello");
        let turn = provider.create_turn(&reqwest::Client::new()).await.unwrap();
        assert_eq!(turn.answer.as_deref(), Some("ok"));
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /chat/completions "));
        assert!(requests[0].contains("authorization: Bearer test-key"));
    }
}
