use std::env;
use std::process::Command;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::provider::{api_error_message, compact_history, send_with_retry};

const VISUAL_SANITIZED: &str = "[Visual Asset Sanitized for Tier-1 Reasoning]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Text(String),
    Multipart(Vec<Value>),
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Content,
    pub tool_call_id: Option<String>,
    pub tool_calls: Option<Vec<Value>>,
    pub reasoning: Option<String>,
    pub reasoning_details: Option<Vec<Value>>,
    source_model: Option<String>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Text(text.into()),
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            reasoning_details: None,
            source_model: None,
        }
    }

    pub fn user_image(prompt: impl Into<String>, data_url: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Content::Multipart(vec![
                json!({"type": "text", "text": prompt.into()}),
                json!({"type": "image_url", "image_url": {"url": data_url.into()}}),
            ]),
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            reasoning_details: None,
            source_model: None,
        }
    }

    pub fn assistant_text(text: impl Into<String>, source_model: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Content::Text(text.into()),
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            reasoning_details: None,
            source_model: Some(source_model.into()),
        }
    }

    pub fn tool_result(call_id: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Content::Text(result.into()),
            tool_call_id: Some(call_id.into()),
            tool_calls: None,
            reasoning: None,
            reasoning_details: None,
            source_model: None,
        }
    }

    fn from_api(value: &Value, source_model: &str) -> Result<Self> {
        let object = value
            .as_object()
            .context("OpenRouter assistant message must be an object")?;
        let content = match object.get("content") {
            None | Some(Value::Null) => Content::Empty,
            Some(Value::String(text)) => Content::Text(text.clone()),
            Some(Value::Array(parts)) => Content::Multipart(parts.clone()),
            Some(_) => bail!("OpenRouter assistant content must be text, multipart, or null"),
        };
        let tool_calls = match object.get("tool_calls") {
            None | Some(Value::Null) => None,
            Some(Value::Array(calls)) => Some(calls.clone()),
            Some(_) => bail!("OpenRouter assistant tool_calls must be an array"),
        };
        let reasoning_details = match object.get("reasoning_details") {
            None | Some(Value::Null) => None,
            Some(Value::Array(details)) => Some(details.clone()),
            Some(_) => bail!("OpenRouter reasoning_details must be an array"),
        };
        Ok(Self {
            role: Role::Assistant,
            content,
            tool_call_id: None,
            tool_calls,
            reasoning: object
                .get("reasoning")
                .or_else(|| object.get("reasoning_content"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            reasoning_details,
            source_model: Some(source_model.to_owned()),
        })
    }

    fn to_api_value(&self, target_model: &str, sanitize_visuals: bool) -> Value {
        let content = if sanitize_visuals {
            sanitize_content(&self.content)
        } else {
            content_to_value(&self.content)
        };
        let mut object = Map::new();
        object.insert(
            "role".to_owned(),
            Value::String(self.role.as_str().to_owned()),
        );
        object.insert("content".to_owned(), content);
        if let Some(call_id) = &self.tool_call_id {
            object.insert("tool_call_id".to_owned(), Value::String(call_id.clone()));
        }
        if let Some(tool_calls) = &self.tool_calls {
            let tool_calls = if sanitize_visuals {
                tool_calls.iter().map(sanitize_value).collect()
            } else {
                tool_calls.clone()
            };
            object.insert("tool_calls".to_owned(), Value::Array(tool_calls));
        }

        // Reasoning blocks must be replayed unmodified and only to the model
        // that produced them. Cross-model encrypted blocks are not portable.
        if self.source_model.as_deref() == Some(target_model) {
            if let Some(details) = &self.reasoning_details {
                object.insert(
                    "reasoning_details".to_owned(),
                    Value::Array(details.clone()),
                );
            } else if let Some(reasoning) = &self.reasoning {
                object.insert("reasoning".to_owned(), Value::String(reasoning.clone()));
            }
        }
        Value::Object(object)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelTier {
    Tier1,
    Tier2,
}

#[derive(Clone, Debug)]
pub struct HostContext {
    pub macos_version: String,
    pub architecture: String,
    pub shell: String,
}

impl HostContext {
    pub fn collect() -> Self {
        Self {
            macos_version: command_output("/usr/bin/sw_vers", &["-productVersion"]),
            architecture: command_output("/usr/bin/uname", &["-m"]),
            shell: env::var("SHELL").unwrap_or_else(|_| "unknown".to_owned()),
        }
    }

    fn system_suffix(&self) -> String {
        format!(
            "\nTrusted local host context:\n- OS: macOS {}\n- architecture: {}\n- shell: {}\nUse macOS-native paths and prefer Finn's audited tools. General shell execution may be unavailable and must never be used to bypass a denied capability.",
            self.macos_version, self.architecture, self.shell
        )
    }
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().chars().take(128).collect())
        .filter(|output: &String| !output.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

#[derive(Clone, Debug)]
pub struct OrchestratorConfig {
    pub api_key: String,
    pub base_url: String,
    pub tier1_model: String,
    pub tier2_model: String,
    pub reasoning_effort: String,
}

#[derive(Clone, Debug)]
struct OrchestratorState {
    history: Vec<Message>,
    active_tier: ModelTier,
    revision: u64,
}

struct OrchestratorInner {
    config: OrchestratorConfig,
    host: HostContext,
    state: RwLock<OrchestratorState>,
    turn_gate: Mutex<()>,
}

#[derive(Clone)]
pub struct FinnOrchestrator {
    inner: Arc<OrchestratorInner>,
}

#[derive(Clone, Debug)]
pub struct OrchestratorResponse {
    pub model: String,
    pub response: Value,
}

impl FinnOrchestrator {
    pub fn new(config: OrchestratorConfig) -> Self {
        Self::with_host_context(config, HostContext::collect())
    }

    pub fn with_host_context(config: OrchestratorConfig, host: HostContext) -> Self {
        Self {
            inner: Arc::new(OrchestratorInner {
                config,
                host,
                state: RwLock::new(OrchestratorState {
                    history: Vec::new(),
                    active_tier: ModelTier::Tier1,
                    revision: 0,
                }),
                turn_gate: Mutex::new(()),
            }),
        }
    }

    /// Creates an independent state snapshot for transactional rollback.
    pub fn fork(&self) -> Self {
        let state = self.read_state().clone();
        Self {
            inner: Arc::new(OrchestratorInner {
                config: self.inner.config.clone(),
                host: self.inner.host.clone(),
                state: RwLock::new(state),
                turn_gate: Mutex::new(()),
            }),
        }
    }

    pub fn push_user(&self, text: &str) {
        let tier = route_text(text);
        let mut state = self.write_state();
        state.active_tier = tier;
        state.history.push(Message::user_text(text));
        compact_history(&mut state.history, is_user_message);
        state.revision = state.revision.wrapping_add(1);
    }

    pub fn push_user_image(&self, prompt: &str, data_url: &str) {
        let mut state = self.write_state();
        state.active_tier = ModelTier::Tier2;
        state.history.push(Message::user_image(prompt, data_url));
        compact_history(&mut state.history, is_user_message);
        state.revision = state.revision.wrapping_add(1);
    }

    pub fn push_assistant(&self, answer: &str) {
        let model = self.active_model();
        let mut state = self.write_state();
        state.history.push(Message::assistant_text(answer, model));
        state.revision = state.revision.wrapping_add(1);
    }

    pub fn push_tool_result(&self, call_id: &str, result: &str) {
        let mut state = self.write_state();
        state.history.push(Message::tool_result(call_id, result));
        state.revision = state.revision.wrapping_add(1);
    }

    pub fn active_model(&self) -> String {
        let state = self.read_state();
        self.model_for_tier(state.active_tier).to_owned()
    }

    pub async fn create_turn<F>(
        &self,
        client: &Client,
        instructions_for_model: F,
        tools: Vec<Value>,
        sink: crate::provider::TextSink<'_>,
    ) -> Result<OrchestratorResponse>
    where
        F: FnOnce(&str) -> String,
    {
        // Only one API turn may append to a unified history at a time.
        let _turn = self.inner.turn_gate.lock().await;
        // Snapshot routing state and serialize the history to its wire form in a
        // single read-lock scope. Serializing here (rather than cloning the
        // history and serializing later) avoids duplicating potentially large
        // base64 image payloads, and taking one consistent lock avoids any
        // window where tier and history could disagree.
        let (model, revision, tier, request_messages) = {
            let state = self.read_state();
            let tier = state.active_tier;
            let model = self.model_for_tier(tier).to_owned();
            let base_instructions = instructions_for_model(&model);
            let sanitize_visuals = tier == ModelTier::Tier1;
            let mut request_messages = Vec::with_capacity(state.history.len() + 1);
            request_messages.push(json!({
                "role": "system",
                "content": format!("{base_instructions}{}", self.inner.host.system_suffix())
            }));
            request_messages.extend(
                state
                    .history
                    .iter()
                    .map(|message| message.to_api_value(&model, sanitize_visuals)),
            );
            (model, state.revision, tier, request_messages)
        };

        let mut request = json!({
            "model": model,
            "messages": request_messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if !tools.is_empty() {
            request["tools"] = Value::Array(tools);
            request["tool_choice"] = Value::String("auto".to_owned());
            request["parallel_tool_calls"] = Value::Bool(false);
        }
        if tier == ModelTier::Tier1 {
            request["reasoning"] = reasoning_config(&self.inner.config.reasoning_effort);
        }
        let request = client
            .post(format!(
                "{}/chat/completions",
                self.inner.config.base_url.trim_end_matches('/')
            ))
            .bearer_auth(&self.inner.config.api_key)
            .json(&request);
        let http = send_with_retry(request, "the OpenRouter Chat Completions API").await?;
        let status = http.status();
        if !status.is_success() {
            let body = http
                .text()
                .await
                .context("cannot read the OpenRouter response")?;
            bail!(
                "OpenRouter API returned {status}: {}",
                api_error_message(&body)
            );
        }
        let response = accumulate_stream(http, sink).await?;
        let assistant_value = response
            .pointer("/choices/0/message")
            .context("OpenRouter response did not contain choices[0].message")?;
        let assistant = Message::from_api(assistant_value, &model)?;
        let mut state = self.write_state();
        if state.revision != revision {
            bail!(
                "Finn's conversation changed while an OpenRouter request was in flight; the stale response was discarded"
            );
        }
        state.history.push(assistant);
        state.revision = state.revision.wrapping_add(1);
        Ok(OrchestratorResponse { model, response })
    }

    fn model_for_tier(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Tier1 => &self.inner.config.tier1_model,
            ModelTier::Tier2 => &self.inner.config.tier2_model,
        }
    }

    fn read_state(&self) -> RwLockReadGuard<'_, OrchestratorState> {
        self.inner
            .state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, OrchestratorState> {
        self.inner
            .state
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn is_user_message(message: &Message) -> bool {
    message.role == Role::User
}

/// Reassembles a non-streaming Chat Completions response object from an SSE
/// stream, forwarding assistant content deltas to `sink` as they arrive. Tool
/// calls stream as indexed fragments whose `arguments` are concatenated.
async fn accumulate_stream(
    response: reqwest::Response,
    sink: crate::provider::TextSink<'_>,
) -> Result<Value> {
    let mut id = String::new();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut reasoning_details: Vec<Value> = Vec::new();
    let mut annotations: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<ToolCallAccumulator> = Vec::new();
    let mut usage = Value::Null;

    crate::provider::read_sse(response, |data| {
        let event: Value =
            serde_json::from_str(data).context("OpenRouter stream returned invalid JSON")?;
        if let Some(event_id) = event.get("id").and_then(Value::as_str)
            && id.is_empty()
        {
            id = event_id.to_owned();
        }
        if let Some(event_usage) = event.get("usage")
            && !event_usage.is_null()
        {
            usage = event_usage.clone();
        }
        let Some(delta) = event.pointer("/choices/0/delta") else {
            return Ok(());
        };
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            content.push_str(text);
            sink(text);
        }
        if let Some(text) = delta
            .get("reasoning")
            .or_else(|| delta.get("reasoning_content"))
            .and_then(Value::as_str)
        {
            reasoning.push_str(text);
        }
        if let Some(details) = delta.get("reasoning_details").and_then(Value::as_array) {
            reasoning_details.extend(details.iter().cloned());
        }
        if let Some(items) = delta.get("annotations").and_then(Value::as_array) {
            annotations.extend(items.iter().cloned());
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                while tool_calls.len() <= index {
                    tool_calls.push(ToolCallAccumulator::default());
                }
                let slot = &mut tool_calls[index];
                if let Some(kind) = call.get("type").and_then(Value::as_str) {
                    slot.kind = kind.to_owned();
                }
                if let Some(call_id) = call.get("id").and_then(Value::as_str) {
                    slot.id = call_id.to_owned();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    slot.name = name.to_owned();
                    if slot.kind.is_empty() {
                        slot.kind = "function".to_owned();
                    }
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    slot.arguments.push_str(args);
                }
            }
        }
        Ok(())
    })
    .await?;

    let source_suffix = citation_suffix(&content, &annotations);
    if !source_suffix.is_empty() {
        sink(&source_suffix);
        content.push_str(&source_suffix);
    }

    let mut message = serde_json::Map::new();
    message.insert("role".to_owned(), Value::String("assistant".to_owned()));
    message.insert(
        "content".to_owned(),
        if content.is_empty() {
            Value::Null
        } else {
            Value::String(content)
        },
    );
    if !reasoning.is_empty() {
        message.insert("reasoning".to_owned(), Value::String(reasoning));
    }
    if !reasoning_details.is_empty() {
        message.insert(
            "reasoning_details".to_owned(),
            Value::Array(reasoning_details),
        );
    }
    if !annotations.is_empty() {
        message.insert("annotations".to_owned(), Value::Array(annotations));
    }
    if !tool_calls.is_empty() {
        let calls = tool_calls
            .into_iter()
            .filter(|call| call.kind == "function" && !call.id.is_empty() && !call.name.is_empty())
            .map(|call| {
                json!({
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": call.arguments}
                })
            })
            .collect::<Vec<_>>();
        message.insert("tool_calls".to_owned(), Value::Array(calls));
    }

    Ok(json!({
        "id": id,
        "choices": [{"message": Value::Object(message)}],
        "usage": usage,
    }))
}

fn citation_suffix(content: &str, annotations: &[Value]) -> String {
    let mut sources = Vec::<(String, String)>::new();
    for annotation in annotations {
        let citation = annotation.get("url_citation").or_else(|| {
            (annotation.get("type").and_then(Value::as_str) == Some("url_citation"))
                .then_some(annotation)
        });
        let Some(citation) = citation else {
            continue;
        };
        let Some(url) = citation.get("url").and_then(Value::as_str) else {
            continue;
        };
        if content.contains(url) || sources.iter().any(|(_, existing)| existing == url) {
            continue;
        }
        let title = citation
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .unwrap_or(url)
            .replace(['[', ']'], "");
        sources.push((title, url.to_owned()));
    }
    if sources.is_empty() {
        return String::new();
    }
    let links = sources
        .into_iter()
        .take(10)
        .map(|(title, url)| format!("- [{title}]({url})"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("\n\nSources:\n{links}")
}

#[derive(Default)]
struct ToolCallAccumulator {
    kind: String,
    id: String,
    name: String,
    arguments: String,
}

fn reasoning_config(effort: &str) -> Value {
    if matches!(effort, "high" | "xhigh") {
        json!({"effort": effort, "exclude": false})
    } else {
        json!({"enabled": true, "exclude": false})
    }
}

fn route_text(text: &str) -> ModelTier {
    let text = text.to_ascii_lowercase();
    let contains_visual_url = (text.contains("http://") || text.contains("https://"))
        && [".png", ".jpg", ".jpeg", ".gif", ".webp"]
            .iter()
            .any(|extension| text.contains(extension));
    let requests_visual_verification = [
        "visual verification",
        "verify the layout",
        "verify layout",
        "inspect the screenshot",
        "analyze the screenshot",
        "bounding box",
        "screen coordinates",
        "gui coordinates",
    ]
    .iter()
    .any(|phrase| text.contains(phrase));
    if text.contains("data:image/") || contains_visual_url || requests_visual_verification {
        ModelTier::Tier2
    } else {
        ModelTier::Tier1
    }
}

fn content_to_value(content: &Content) -> Value {
    match content {
        Content::Text(text) => Value::String(text.clone()),
        Content::Multipart(parts) => Value::Array(parts.clone()),
        Content::Empty => Value::Null,
    }
}

fn sanitize_content(content: &Content) -> Value {
    match content {
        Content::Text(text) => sanitize_value(&Value::String(text.clone())),
        Content::Multipart(parts) => {
            let mut sanitized = Vec::new();
            let mut inserted_marker = false;
            for part in parts {
                if is_image_object(part) {
                    if !inserted_marker {
                        sanitized.push(json!({"type": "text", "text": VISUAL_SANITIZED}));
                        inserted_marker = true;
                    }
                } else {
                    sanitized.push(sanitize_value(part));
                }
            }
            Value::Array(sanitized)
        }
        Content::Empty => Value::Null,
    }
}

fn sanitize_value(value: &Value) -> Value {
    match value {
        Value::String(text) => sanitize_string(text),
        Value::Array(values) => Value::Array(values.iter().map(sanitize_value).collect()),
        Value::Object(object) if is_image_object(value) => {
            json!({"type": "text", "text": VISUAL_SANITIZED})
        }
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), sanitize_value(value)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn sanitize_string(text: &str) -> Value {
    if contains_image_data(text) || looks_like_raw_image_base64(text) {
        return Value::String(VISUAL_SANITIZED.to_owned());
    }
    if let Ok(nested) = serde_json::from_str::<Value>(text)
        && (nested.is_array() || nested.is_object())
    {
        return Value::String(
            serde_json::to_string(&sanitize_value(&nested))
                .unwrap_or_else(|_| VISUAL_SANITIZED.to_owned()),
        );
    }
    Value::String(text.to_owned())
}

fn is_image_object(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "image" | "image_url" | "input_image" | "image_file"))
        || object.contains_key("image_url")
        || object.contains_key("b64_json")
        || object.contains_key("image_bytes")
        || object.contains_key("image_data")
}

fn contains_image_data(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("data:image/") && lower.contains(";base64,")
}

fn looks_like_raw_image_base64(text: &str) -> bool {
    let compact = text.trim();
    compact.len() >= 256
        && ["iVBORw0KGgo", "/9j/", "R0lGOD", "UklGR", "SUkqA", "TU0AK"]
            .iter()
            .any(|prefix| compact.starts_with(prefix))
        && compact
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_support;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    fn config(base_url: String) -> OrchestratorConfig {
        OrchestratorConfig {
            api_key: "test-key".to_owned(),
            base_url,
            tier1_model: "z-ai/glm-5.2".to_owned(),
            tier2_model: "z-ai/glm-5v-turbo".to_owned(),
            reasoning_effort: "high".to_owned(),
        }
    }

    #[test]
    fn appends_unique_annotation_sources_missing_from_content() {
        let annotations = vec![
            json!({
                "type": "url_citation",
                "url_citation": {
                    "url": "https://example.com/article",
                    "title": "Example [Article]"
                }
            }),
            json!({
                "type": "url_citation",
                "url_citation": {
                    "url": "https://example.com/article",
                    "title": "Duplicate"
                }
            }),
        ];
        let suffix = citation_suffix("Grounded answer", &annotations);
        assert_eq!(
            suffix,
            "\n\nSources:\n- [Example Article](https://example.com/article)"
        );
        assert!(citation_suffix("See https://example.com/article", &annotations).is_empty());
    }

    fn host() -> HostContext {
        HostContext {
            macos_version: "15.5".to_owned(),
            architecture: "arm64".to_owned(),
            shell: "/bin/zsh".to_owned(),
        }
    }

    #[test]
    fn routes_visual_payloads_and_explicit_visual_work() {
        assert_eq!(route_text("hello"), ModelTier::Tier1);
        assert_eq!(
            route_text("verify the layout before delivery"),
            ModelTier::Tier2
        );
        assert_eq!(
            route_text("inspect https://example.com/screen.png"),
            ModelTier::Tier2
        );
        assert_eq!(route_text("data:image/png;base64,YQ=="), ModelTier::Tier2);
    }

    #[test]
    fn deeply_sanitizes_images_for_tier1() {
        let message = Message {
            role: Role::User,
            content: Content::Multipart(vec![
                json!({"type": "text", "text": "keep me"}),
                json!({"type": "image_url", "image_url": {"url": "https://x/y.png"}}),
                json!({"nested": {"payload": "data:image/png;base64,AAAA"}}),
            ]),
            tool_call_id: None,
            tool_calls: Some(vec![json!({
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "inspect",
                    "arguments": "{\"image_url\":{\"url\":\"data:image/jpeg;base64,BBBB\"}}"
                }
            })]),
            reasoning: None,
            reasoning_details: None,
            source_model: None,
        };
        let sanitized = message.to_api_value("z-ai/glm-5.2", true);
        let serialized = serde_json::to_string(&sanitized).unwrap();
        assert!(serialized.contains("keep me"));
        assert!(serialized.contains(VISUAL_SANITIZED));
        assert!(!serialized.contains("image_url"));
        assert!(!serialized.contains("base64"));
    }

    #[test]
    fn image_user_messages_never_contain_tool_calls() {
        let message = Message::user_image("inspect", "data:image/png;base64,YQ==");
        let payload = message.to_api_value("z-ai/glm-5v-turbo", false);
        assert_eq!(payload["role"], "user");
        assert!(payload.get("tool_calls").is_none());
        assert_eq!(payload["content"][1]["type"], "image_url");
    }

    #[test]
    fn history_compaction_starts_at_a_user_message() {
        let mut history = (0..140)
            .flat_map(|index| {
                [
                    Message::user_text(format!("user {index}")),
                    Message::assistant_text(format!("assistant {index}"), "model"),
                ]
            })
            .collect::<Vec<_>>();
        compact_history(&mut history, is_user_message);
        assert!(history.len() <= crate::provider::MAX_HISTORY_ITEMS);
        assert_eq!(
            history.first().map(|message| message.role),
            Some(Role::User)
        );
    }

    #[test]
    fn preserves_reasoning_only_for_its_source_model() {
        let details = vec![json!({
            "type": "reasoning.text",
            "text": "opaque reasoning",
            "id": "r1",
            "format": "unknown",
            "index": 0
        })];
        let message = Message {
            role: Role::Assistant,
            content: Content::Text("answer".to_owned()),
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            reasoning_details: Some(details.clone()),
            source_model: Some("z-ai/glm-5.2".to_owned()),
        };
        assert_eq!(
            message.to_api_value("z-ai/glm-5.2", true)["reasoning_details"],
            Value::Array(details)
        );
        assert!(
            message
                .to_api_value("z-ai/glm-5v-turbo", false)
                .get("reasoning_details")
                .is_none()
        );
    }

    #[tokio::test]
    async fn routes_requests_and_replays_reasoning_details() {
        // Tool-call turn with streamed reasoning_details and an indexed tool call.
        let first = test_support::sse_body(&[
            json!({"id": "gen_1", "choices": [{"delta": {
                "reasoning_details": [{
                    "type": "reasoning.text", "text": "reason", "id": "r1",
                    "format": "unknown", "index": 0
                }]
            }}]}),
            json!({"id": "gen_1", "choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": "call_1", "type": "function",
                "function": {"name": "path_status", "arguments": "{\"path\":\"~/Desktop\"}"}
            }]}}]}),
            json!({"id": "gen_1", "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}),
        ]);
        let second = test_support::sse_text("gen_2", "done");
        let (base_url, server) =
            test_support::mock_http_server(vec![("200 OK", first), ("200 OK", second)]).await;
        let orchestrator = FinnOrchestrator::with_host_context(config(base_url), host());
        orchestrator.push_user("check the Desktop");
        let client = Client::new();
        let mut noop = |_: &str| {};
        let first_turn = orchestrator
            .create_turn(
                &client,
                |_| "instructions".to_owned(),
                Vec::new(),
                &mut noop,
            )
            .await
            .unwrap();
        assert_eq!(first_turn.model, "z-ai/glm-5.2");
        orchestrator.push_tool_result("call_1", "exists: true");
        orchestrator
            .create_turn(
                &client,
                |_| "instructions".to_owned(),
                Vec::new(),
                &mut noop,
            )
            .await
            .unwrap();

        let requests = server.await.unwrap();
        assert!(requests[0].contains("macOS 15.5"));
        assert!(requests[0].contains("\"model\":\"z-ai/glm-5.2\""));
        assert!(requests[1].contains("\"reasoning_details\""));
        assert!(requests[1].contains("\"id\":\"r1\""));
    }

    #[tokio::test]
    async fn sanitizes_history_when_returning_to_tier1() {
        let vision = test_support::sse_text("vision", "I saw a chart.");
        let text = test_support::sse_text("text", "Continuing.");
        let (base_url, server) =
            test_support::mock_http_server(vec![("200 OK", vision), ("200 OK", text)]).await;
        let orchestrator = FinnOrchestrator::with_host_context(config(base_url), host());
        orchestrator.push_user_image("inspect", "data:image/png;base64,YQ==");
        let client = Client::new();
        let mut noop = |_: &str| {};
        let visual_turn = orchestrator
            .create_turn(
                &client,
                |_| "instructions".to_owned(),
                Vec::new(),
                &mut noop,
            )
            .await
            .unwrap();
        assert_eq!(visual_turn.model, "z-ai/glm-5v-turbo");

        orchestrator.push_user("continue with text reasoning");
        let text_turn = orchestrator
            .create_turn(
                &client,
                |_| "instructions".to_owned(),
                Vec::new(),
                &mut noop,
            )
            .await
            .unwrap();
        assert_eq!(text_turn.model, "z-ai/glm-5.2");

        let requests = server.await.unwrap();
        assert!(requests[0].contains("data:image/png;base64,YQ=="));
        assert!(!requests[1].contains("data:image/png"));
        assert!(requests[1].contains(VISUAL_SANITIZED));
    }

    #[tokio::test]
    async fn streams_text_deltas_and_reassembles_tool_calls() {
        // A turn that streams two text deltas and a tool call whose arguments
        // arrive in fragments across several events.
        let events = test_support::sse_body(&[
            json!({"id": "gen_s", "choices": [{"delta": {"content": "Hel"}}]}),
            json!({"id": "gen_s", "choices": [{"delta": {"content": "lo"}}]}),
            json!({"id": "gen_s", "choices": [{"delta": {"tool_calls": [{
                "index": 0, "id": "server_search", "type": "openrouter:web_search"
            }]}}]}),
            json!({"id": "gen_s", "choices": [{"delta": {"tool_calls": [{
                "index": 1, "id": "call_x", "type": "function",
                "function": {"name": "path_status", "arguments": "{\"pa"}
            }]}}]}),
            json!({"id": "gen_s", "choices": [{"delta": {"tool_calls": [{
                "index": 1, "function": {"arguments": "th\":\"~\"}"}
            }]}}]}),
            json!({"id": "gen_s", "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 2, "completion_tokens": 3, "total_tokens": 5}}),
        ]);
        let (base_url, server) = test_support::mock_http_server(vec![("200 OK", events)]).await;
        let orchestrator = FinnOrchestrator::with_host_context(config(base_url), host());
        orchestrator.push_user("check home");
        let mut streamed = String::new();
        let mut sink = |delta: &str| streamed.push_str(delta);
        let response = orchestrator
            .create_turn(
                &Client::new(),
                |_| "instructions".to_owned(),
                Vec::new(),
                &mut sink,
            )
            .await
            .unwrap();

        assert_eq!(streamed, "Hello");
        let message = response.response.pointer("/choices/0/message").unwrap();
        assert_eq!(message["content"], "Hello");
        assert_eq!(message["tool_calls"].as_array().unwrap().len(), 1);
        assert_eq!(message["tool_calls"][0]["id"], "call_x");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"~\"}"
        );
        assert_eq!(response.response["usage"]["total_tokens"], 5);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn discards_a_response_if_history_changes_in_flight() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_seen_tx, request_seen_rx) = oneshot::channel();
        let (respond_tx, respond_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let bytes_read = stream.read(&mut request).await.unwrap();
            assert!(bytes_read > 0);
            request_seen_tx.send(()).unwrap();
            respond_rx.await.unwrap();
            let body =
                "data: {\"choices\":[{\"delta\":{\"content\":\"stale\"}}]}\n\ndata: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });

        let orchestrator =
            FinnOrchestrator::with_host_context(config(format!("http://{address}")), host());
        orchestrator.push_user("first request");
        let request_orchestrator = orchestrator.clone();
        let request = tokio::spawn(async move {
            let mut noop = |_: &str| {};
            request_orchestrator
                .create_turn(
                    &Client::new(),
                    |_| "instructions".to_owned(),
                    Vec::new(),
                    &mut noop,
                )
                .await
        });
        request_seen_rx.await.unwrap();
        orchestrator.push_user("newer request");
        respond_tx.send(()).unwrap();

        let error = request.await.unwrap().unwrap_err();
        assert!(error.to_string().contains("stale response was discarded"));
        server.await.unwrap();
    }
}
