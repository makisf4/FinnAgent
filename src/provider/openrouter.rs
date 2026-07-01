use anyhow::{Context, Result, bail};
use serde_json::{Map, Value, json};

use super::{ModelTurn, ToolCall, Usage};
use crate::config::Config;
use crate::orchestrator::{FinnOrchestrator, OrchestratorConfig};

#[derive(Clone)]
pub struct OpenRouter {
    orchestrator: FinnOrchestrator,
    reasoning_effort: String,
}

impl OpenRouter {
    pub fn new(config: &Config) -> Self {
        let tier2_model = config
            .vision_model
            .clone()
            .unwrap_or_else(|| config.model.clone());
        Self {
            orchestrator: FinnOrchestrator::new(OrchestratorConfig {
                api_key: config.api_key.clone(),
                base_url: config.base_url.clone(),
                tier1_model: config.model.clone(),
                tier2_model,
                reasoning_effort: config.reasoning_effort.clone(),
            }),
            reasoning_effort: config.reasoning_effort.clone(),
        }
    }

    pub fn push_user(&mut self, task: &str) {
        self.orchestrator.push_user(task);
    }

    pub fn push_user_image(&mut self, prompt: &str, data_url: &str) {
        self.orchestrator.push_user_image(prompt, data_url);
    }

    pub fn push_assistant(&mut self, answer: &str) {
        self.orchestrator.push_assistant(answer);
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str) {
        self.orchestrator.push_tool_result(call_id, result);
    }

    pub fn fork(&self) -> Self {
        Self {
            orchestrator: self.orchestrator.fork(),
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }

    pub async fn create_turn(
        &mut self,
        client: &reqwest::Client,
        tools: Vec<Value>,
        sink: super::TextSink<'_>,
    ) -> Result<ModelTurn> {
        let reasoning_effort = self.reasoning_effort.clone();
        let response = self
            .orchestrator
            .create_turn(
                client,
                |model| crate::agent::instructions(model, &reasoning_effort),
                chat_tool_definitions(tools),
                sink,
            )
            .await?;
        let mut turn = parse_response(&response.response)?;
        turn.model = response.model;
        Ok(turn)
    }
}

fn chat_tool_definitions(tools: Vec<Value>) -> Vec<Value> {
    tools
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
        model: String::new(),
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
        assert!(turn.model.is_empty());
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
        let body = test_support::sse_text("gen_mock", "ok");
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
        let mut streamed = String::new();
        let mut sink = |delta: &str| streamed.push_str(delta);
        let turn = provider
            .create_turn(&reqwest::Client::new(), Vec::new(), &mut sink)
            .await
            .unwrap();
        assert_eq!(turn.answer.as_deref(), Some("ok"));
        assert_eq!(turn.model, "router/test");
        assert_eq!(streamed, "ok");
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /chat/completions "));
        assert!(requests[0].contains("authorization: Bearer test-key"));
        assert!(requests[0].contains("\"stream\":true"));
    }
}
