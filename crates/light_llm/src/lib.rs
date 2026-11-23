//! A small client for locally hosted language models.
//!
//! It speaks Ollama's native API, which unlike its OpenAI-compatible endpoint lets callers set the
//! context window per request.

mod ndjson;

use std::time::Duration;

use async_compat::CompatExt as _;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use ndjson::NdjsonDecoder;

/// Default address of a local Ollama server.
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";

/// How long to wait for Ollama to answer a quick metadata request.
const METADATA_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not reach the model server: {0}")]
    Http(#[from] reqwest::Error),
    #[error("the model server answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("the model server sent invalid data: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("the model server reported an error: {0}")]
    Server(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// For `Role::Tool` messages, the name of the tool whose result this is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, content)
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, content)
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, content)
    }

    pub fn tool_result(tool_name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_name: Some(tool_name.into()),
            ..Self::new(Role::Tool, content)
        }
    }

    fn new(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: Vec::new(),
            tool_name: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub function: FunctionCall,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// A tool the model may call, described with a JSON Schema for its arguments.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ToolFunction,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

impl Tool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            kind: "function",
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }

    pub fn name(&self) -> &str {
        &self.function.name
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<ChatOptions>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ChatOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
}

/// One line of a streamed `/api/chat` response.
#[derive(Debug, Clone, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    message: Option<ChunkMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    done_reason: Option<String>,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChunkMessage {
    #[serde(default)]
    content: String,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

/// An increment of a streamed chat response.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    ContentDelta(String),
    ThinkingDelta(String),
    ToolCall(ToolCall),
    Done(Usage),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub output_tokens: u32,
    pub done_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<ModelInfo>,
}

/// Turns one decoded line into the events it carries.
fn events_from_chunk(chunk: ChatChunk) -> Result<Vec<ChatEvent>, Error> {
    if let Some(error) = chunk.error {
        return Err(Error::Server(error));
    }
    let mut events = Vec::new();
    if let Some(message) = chunk.message {
        if let Some(thinking) = message.thinking.filter(|thinking| !thinking.is_empty()) {
            events.push(ChatEvent::ThinkingDelta(thinking));
        }
        if !message.content.is_empty() {
            events.push(ChatEvent::ContentDelta(message.content));
        }
        events.extend(message.tool_calls.into_iter().map(ChatEvent::ToolCall));
    }
    if chunk.done {
        events.push(ChatEvent::Done(Usage {
            prompt_tokens: chunk.prompt_eval_count.unwrap_or_default(),
            output_tokens: chunk.eval_count.unwrap_or_default(),
            done_reason: chunk.done_reason,
        }));
    }
    Ok(events)
}

/// Decodes a stream of NDJSON byte chunks into chat events.
pub fn decode_chat_stream<S, E>(bytes: S) -> impl Stream<Item = Result<ChatEvent, Error>>
where
    S: Stream<Item = Result<bytes::Bytes, E>>,
    Error: From<E>,
{
    async_stream::stream! {
        let mut decoder = NdjsonDecoder::default();
        let mut bytes = std::pin::pin!(bytes);
        // reqwest needs a Tokio reactor to read the body.
        while let Some(chunk) = bytes.next().compat().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    yield Err(Error::from(error));
                    return;
                }
            };
            for line in decoder.push(&chunk) {
                match serde_json::from_slice::<ChatChunk>(&line).map_err(Error::Decode).and_then(events_from_chunk) {
                    Ok(events) => for event in events { yield Ok(event) },
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
        }
        if let Some(line) = decoder.finish() {
            match serde_json::from_slice::<ChatChunk>(&line).map_err(Error::Decode).and_then(events_from_chunk) {
                Ok(events) => for event in events { yield Ok(event) },
                Err(error) => yield Err(error),
            }
        }
    }
}

/// A client for an Ollama server.
#[derive(Clone)]
pub struct OllamaClient {
    base_url: String,
    http: std::sync::Arc<http_client::Client>,
}

impl OllamaClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_http_client(base_url, http_client::Client::new_external())
    }

    pub fn with_http_client(base_url: impl Into<String>, http: http_client::Client) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            http: std::sync::Arc::new(http),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Lists the models installed on the server.
    pub async fn list_models(&self) -> Result<Vec<ModelInfo>, Error> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.base_url))
            .timeout(METADATA_TIMEOUT)
            .send()
            .compat()
            .await?;
        let response = check_status(response).await?;
        let tags: TagsResponse = response.json().compat().await?;
        Ok(tags.models)
    }

    /// Starts a streamed chat completion.
    pub async fn chat_stream(
        &self,
        request: &ChatRequest,
    ) -> Result<BoxStream<'static, Result<ChatEvent, Error>>, Error> {
        let response = self
            .http
            .post(format!("{}/api/chat", self.base_url))
            .json(request)
            .send()
            .compat()
            .await?;
        let response = check_status(response).await?;
        Ok(decode_chat_stream(response.bytes_stream()).boxed())
    }
}

async fn check_status(response: http_client::Response) -> Result<http_client::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().compat().await.unwrap_or_default();
    // Ollama reports errors as {"error": "..."}; fall back to the raw body otherwise.
    let body = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
        .unwrap_or(body);
    Err(Error::Status {
        status: status.as_u16(),
        body,
    })
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
