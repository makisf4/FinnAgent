mod openai;
mod openrouter;

use anyhow::Result;
use anyhow::{Context, bail};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde_json::Value;
use tokio::time::{Duration, sleep};

use crate::config::{Config, Provider};

pub use openai::OpenAi;
pub use openrouter::OpenRouter;

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub struct ModelTurn {
    pub response_id: String,
    pub usage: Usage,
    pub tool_calls: Vec<ToolCall>,
    pub answer: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone)]
pub enum Backend {
    OpenAi(OpenAi),
    OpenRouter(OpenRouter),
}

pub(super) async fn send_with_retry(request: RequestBuilder, service: &str) -> Result<Response> {
    const ATTEMPTS: usize = 3;
    for attempt in 0..ATTEMPTS {
        let current = request
            .try_clone()
            .context("cannot clone HTTP request for retry")?;
        match current.send().await {
            Ok(response)
                if attempt + 1 < ATTEMPTS
                    && (response.status() == StatusCode::TOO_MANY_REQUESTS
                        || response.status().is_server_error()) =>
            {
                sleep(retry_delay(attempt)).await;
            }
            Ok(response) => return Ok(response),
            Err(error) if attempt + 1 < ATTEMPTS && error.is_timeout() => {
                sleep(retry_delay(attempt)).await;
            }
            Err(error) if attempt + 1 < ATTEMPTS && error.is_connect() => {
                sleep(retry_delay(attempt)).await;
            }
            Err(error) => return Err(error).with_context(|| format!("cannot reach {service}")),
        }
    }
    bail!("cannot reach {service} after {ATTEMPTS} attempts")
}

fn retry_delay(attempt: usize) -> Duration {
    Duration::from_millis(500 * (1_u64 << attempt))
}

#[cfg(test)]
pub(crate) mod test_support {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    pub async fn mock_http_server(
        responses: Vec<(&'static str, &'static str)>,
    ) -> (String, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = vec![0_u8; 64 * 1024];
                let count = stream.read(&mut buffer).await.unwrap();
                requests.push(String::from_utf8_lossy(&buffer[..count]).into_owned());
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (format!("http://{address}"), server)
    }
}

impl Backend {
    pub fn new(config: &Config) -> Self {
        match config.provider {
            Provider::OpenAi => Self::OpenAi(OpenAi::new(config)),
            Provider::OpenRouter => Self::OpenRouter(OpenRouter::new(config)),
        }
    }

    pub fn push_user(&mut self, task: &str) {
        match self {
            Self::OpenAi(provider) => provider.push_user(task),
            Self::OpenRouter(provider) => provider.push_user(task),
        }
    }

    pub fn push_user_image(&mut self, prompt: &str, data_url: &str) {
        match self {
            Self::OpenAi(provider) => provider.push_user_image(prompt, data_url),
            Self::OpenRouter(provider) => provider.push_user_image(prompt, data_url),
        }
    }

    pub fn push_tool_result(&mut self, call_id: &str, result: &str) {
        match self {
            Self::OpenAi(provider) => provider.push_tool_result(call_id, result),
            Self::OpenRouter(provider) => provider.push_tool_result(call_id, result),
        }
    }

    pub fn push_assistant(&mut self, answer: &str) {
        match self {
            Self::OpenAi(provider) => provider.push_assistant(answer),
            Self::OpenRouter(provider) => provider.push_assistant(answer),
        }
    }

    pub async fn create_turn(&mut self, client: &reqwest::Client) -> Result<ModelTurn> {
        match self {
            Self::OpenAi(provider) => provider.create_turn(client).await,
            Self::OpenRouter(provider) => provider.create_turn(client).await,
        }
    }
}

pub(super) fn api_error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(500).collect())
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn retries_transient_server_errors() {
        let (base_url, server) = test_support::mock_http_server(vec![
            (
                "500 Internal Server Error",
                r#"{"error":{"message":"one"}}"#,
            ),
            ("503 Service Unavailable", r#"{"error":{"message":"two"}}"#),
            ("200 OK", r#"{"ok":true}"#),
        ])
        .await;
        let client = reqwest::Client::new();
        let response = send_with_retry(client.get(base_url), "mock service")
            .await
            .unwrap();
        assert!(response.status().is_success());
        assert_eq!(server.await.unwrap().len(), 3);
    }
}
