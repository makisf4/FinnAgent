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

    pub fn checkpoint(&self) -> Self {
        self.fork()
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
            if tool
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("openrouter:"))
            {
                return tool;
            }
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
    let mut tool_calls = match message.get("tool_calls") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(calls)) => calls
            .iter()
            .map(parse_tool_call)
            .collect::<Result<Vec<_>>>()?,
        Some(_) => bail!("OpenRouter tool-call format mismatch: tool_calls must be an array"),
    };
    let mut answer = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    if tool_calls.is_empty()
        && let Some(content) = answer.as_deref()
        && let Some(dsml_calls) = parse_dsml_tool_calls(content)?
    {
        tool_calls = dsml_calls;
        answer = None;
    }
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let server_tool_use = usage.get("server_tool_use").unwrap_or(&Value::Null);
    let web_search_requests = server_tool_use
        .get("web_search_requests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let web_fetch_requests = server_tool_use
        .get("web_fetch_requests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let has_web_annotations = message
        .get("annotations")
        .and_then(Value::as_array)
        .is_some_and(|annotations| !annotations.is_empty());

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
            web_search_requests,
            web_fetch_requests,
            web_grounded_responses: u64::from(has_web_annotations),
        },
        used_untrusted_server_tool: web_search_requests > 0
            || web_fetch_requests > 0
            || has_web_annotations,
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

fn parse_dsml_tool_calls(content: &str) -> Result<Option<Vec<ToolCall>>> {
    if !content.contains("<｜DSML｜tool_calls>") {
        return Ok(None);
    }

    let mut calls = Vec::new();
    let mut remaining = content;
    while let Some(start) = remaining.find("<｜DSML｜invoke") {
        remaining = &remaining[start..];
        let header_end = remaining
            .find('>')
            .context("OpenRouter DSML tool-call format mismatch: unterminated invoke tag")?;
        let header = &remaining[..header_end + 1];
        let name = attribute_value(header, "name")
            .context("OpenRouter DSML tool-call format mismatch: missing invoke name")?;
        let close = "</｜DSML｜invoke>";
        let body_start = header_end + 1;
        let body_end = remaining[body_start..]
            .find(close)
            .map(|index| body_start + index)
            .context("OpenRouter DSML tool-call format mismatch: unterminated invoke body")?;
        let body = &remaining[body_start..body_end];
        let mut arguments = Map::new();
        let mut body_remaining = body;
        while let Some(param_start) = body_remaining.find("<｜DSML｜parameter") {
            body_remaining = &body_remaining[param_start..];
            let param_header_end = body_remaining
                .find('>')
                .context("OpenRouter DSML tool-call format mismatch: unterminated parameter tag")?;
            let param_header = &body_remaining[..param_header_end + 1];
            let param_name = attribute_value(param_header, "name")
                .context("OpenRouter DSML tool-call format mismatch: missing parameter name")?;
            let param_close = "</｜DSML｜parameter>";
            let value_start = param_header_end + 1;
            let value_end = body_remaining[value_start..]
                .find(param_close)
                .map(|index| value_start + index)
                .context("OpenRouter DSML tool-call format mismatch: unterminated parameter")?;
            let value = decode_xml_entities(&body_remaining[value_start..value_end]);
            arguments.insert(param_name, Value::String(value));
            body_remaining = &body_remaining[value_end + param_close.len()..];
        }
        calls.push(ToolCall {
            id: format!("dsml_call_{}", calls.len() + 1),
            name,
            arguments: Value::Object(arguments).to_string(),
        });
        remaining = &remaining[body_end + close.len()..];
    }

    if calls.is_empty() {
        bail!("OpenRouter DSML tool-call format mismatch: no invoke blocks found");
    }
    Ok(Some(calls))
}

fn attribute_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(decode_xml_entities(&tag[start..end]))
}

fn decode_xml_entities(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
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
    fn parses_deepseek_dsml_tool_call_content() {
        let response = json!({
            "id": "gen-dsml",
            "choices": [{"message": {
                "role": "assistant",
                "content": "<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"run_shell\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">find ~ -type f -size +300M 2&gt;/dev/null</｜DSML｜parameter>\n<｜DSML｜parameter name=\"description\" string=\"true\">Find files larger than 300MB</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>"
            }}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let turn = parse_response(&response).unwrap();
        assert!(turn.answer.is_none());
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, "dsml_call_1");
        assert_eq!(turn.tool_calls[0].name, "run_shell");
        let args: Value = serde_json::from_str(&turn.tool_calls[0].arguments).unwrap();
        assert_eq!(args["cmd"], "find ~ -type f -size +300M 2>/dev/null");
        assert_eq!(args["description"], "Find files larger than 300MB");
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

    #[test]
    fn preserves_openrouter_server_tools_on_the_wire() {
        let tools = chat_tool_definitions(vec![
            json!({
                "type": "openrouter:web_search",
                "parameters": {"max_results": 5}
            }),
            json!({
                "type": "function",
                "name": "path_status",
                "description": "Inspect a path",
                "strict": true,
                "parameters": {"type": "object"}
            }),
        ]);
        assert_eq!(tools[0]["type"], "openrouter:web_search");
        assert_eq!(tools[0]["parameters"]["max_results"], 5);
        assert_eq!(tools[1]["type"], "function");
        assert_eq!(tools[1]["function"]["name"], "path_status");
    }

    #[test]
    fn records_server_web_tool_usage_as_untrusted() {
        let response = json!({
            "choices": [{"message": {"role": "assistant", "content": "grounded answer"}}],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "server_tool_use": {
                    "web_search_requests": 2,
                    "web_fetch_requests": 1
                }
            }
        });
        let turn = parse_response(&response).unwrap();
        assert!(turn.used_untrusted_server_tool);
        assert_eq!(turn.usage.web_search_requests, 2);
        assert_eq!(turn.usage.web_fetch_requests, 1);
    }

    #[test]
    fn citation_annotations_also_mark_web_usage_as_untrusted() {
        let response = json!({
            "choices": [{"message": {
                "role": "assistant",
                "content": "grounded answer",
                "annotations": [{
                    "type": "url_citation",
                    "url_citation": {"url": "https://example.com", "title": "Example"}
                }]
            }}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
        });
        let turn = parse_response(&response).unwrap();
        assert!(turn.used_untrusted_server_tool);
        assert_eq!(turn.usage.web_grounded_responses, 1);
        assert_eq!(turn.usage.web_search_requests, 0);
    }

    #[tokio::test]
    async fn calls_mock_chat_completions_endpoint() {
        let body = test_support::sse_text("gen_mock", "ok");
        let (base_url, server) = test_support::mock_http_server(vec![("200 OK", body)]).await;
        let config = Config {
            api_key: "test-key".to_owned(),
            base_url,
            model: "router/test".to_owned(),
            model_kind: crate::config::ModelKind::Assistant,
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
            .create_turn(
                &reqwest::Client::new(),
                vec![json!({
                    "type": "openrouter:web_search",
                    "parameters": {"max_results": 5}
                })],
                &mut sink,
            )
            .await
            .unwrap();
        assert_eq!(turn.answer.as_deref(), Some("ok"));
        assert_eq!(turn.model, "router/test");
        assert_eq!(streamed, "ok");
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /chat/completions "));
        assert!(requests[0].contains("authorization: Bearer test-key"));
        assert!(requests[0].contains("\"stream\":true"));
        assert!(requests[0].contains("\"type\":\"openrouter:web_search\""));
        assert!(requests[0].contains("\"max_results\":5"));
    }
}
