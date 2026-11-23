//! Serves agent requests with a locally hosted model instead of Warp's servers.
//!
//! The client keeps the whole conversation and resends it with every request, so each turn is
//! translated afresh: the root task's messages become the model's chat history, and the model's
//! streamed reply is turned back into the client actions the Warp server would have sent.

use std::sync::Arc;
use std::time::SystemTime;

use futures_util::StreamExt as _;
use light_llm::{ChatEvent, ChatMessage, ChatOptions, ChatRequest, OllamaClient};
use prost_types::FieldMask;
use uuid::Uuid;
use warp_core::channel::ChannelState;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{AddMessagesToTask, AppendToMessageContent, CreateTask};
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, UserQuery,
};
use warp_multi_agent_api::response_event::{StreamFinished, StreamInit, stream_finished};
use warp_multi_agent_api::{ClientAction, Message, ResponseEvent, Task, client_action};

use super::convert_to::convert_input;
use super::{ConvertToAPITypeError, RequestParams, ResponseStream};
use crate::server::server_api::AIApiError;

const AGENT_OUTPUT_TEXT_PATH: &str = "agent_output.text";
const AGENT_REASONING_TEXT_PATH: &str = "agent_reasoning.reasoning";

const DEFAULT_MODEL: &str = "qwen3:8b";
/// Fits a 7-8B model and its context in 8 GB of VRAM.
const DEFAULT_CONTEXT_WINDOW: u32 = 16_384;

/// Whether agent requests should be served locally.
pub fn is_enabled() -> bool {
    ChannelState::is_offline()
}

/// Where to find the local model and how to run it.
#[derive(Debug, Clone, PartialEq)]
struct LocalModelConfig {
    base_url: String,
    model: String,
    context_window: u32,
}

impl LocalModelConfig {
    fn from_env() -> Self {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        };
        Self {
            base_url: var("LIGHT_WARP_OLLAMA_URL")
                .unwrap_or_else(|| light_llm::DEFAULT_OLLAMA_URL.to_owned()),
            model: var("LIGHT_WARP_OLLAMA_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            context_window: var("LIGHT_WARP_OLLAMA_NUM_CTX")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_CONTEXT_WINDOW),
        }
    }
}

pub async fn generate_local_agent_output(
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let input = convert_input(params.input)?;
    let config = LocalModelConfig::from_env();
    let conversation_id = params
        .conversation_token
        .as_ref()
        .map(|token| token.as_str().to_owned())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let request_id = Uuid::new_v4().to_string();
    let existing_root_task_id = root_task(&params.tasks).map(|task| task.id.clone());
    let task_id = existing_root_task_id
        .clone()
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let mut writer = TurnWriter::new(task_id.clone(), request_id.clone());

    let init = ResponseEvent {
        r#type: Some(api::response_event::Type::Init(StreamInit {
            conversation_id,
            request_id,
            run_id: String::new(),
        })),
    };

    let queries = user_queries(&input);
    if queries.is_empty() {
        // Requests that are not a user turn (e.g. passive suggestions) are not worth running a
        // local model for.
        let events: Vec<Result<_, Arc<AIApiError>>> = vec![
            Ok(init),
            Ok(finished(stream_finished::Reason::Done(
                stream_finished::Done {},
            ))),
        ];
        return Ok(Box::pin(futures::stream::iter(events)));
    }

    let mut opening_actions = Vec::new();
    if existing_root_task_id.is_none() {
        opening_actions.push(action(client_action::Action::CreateTask(CreateTask {
            task: Some(Task {
                id: task_id.clone(),
                ..Default::default()
            }),
        })));
    }
    // The Warp server echoes each input into the task; later turns rely on it being there.
    opening_actions.push(
        writer.add_messages(
            queries
                .iter()
                .map(|query| {
                    writer.new_message(MessageKind::UserQuery(UserQuery {
                        query: query.clone(),
                        context: input.context.clone(),
                        ..Default::default()
                    }))
                })
                .collect(),
        ),
    );

    let mut messages = vec![ChatMessage::system(system_prompt(input.context.as_ref()))];
    messages.extend(history_messages(&params.tasks));
    messages.extend(queries.into_iter().map(ChatMessage::user));
    let request = ChatRequest {
        model: config.model.clone(),
        messages,
        tools: Vec::new(),
        stream: true,
        options: Some(ChatOptions {
            num_ctx: Some(config.context_window),
        }),
    };
    let client = OllamaClient::new(config.base_url.clone());

    let stream = async_stream::stream! {
        yield Ok::<_, Arc<AIApiError>>(init);
        yield Ok(client_actions(opening_actions));

        let mut events = match client.chat_stream(&request).await {
            Ok(events) => events,
            Err(error) => {
                yield Ok(model_error(&config, &error));
                return;
            }
        };
        while let Some(event) = events.next().await {
            match event {
                Ok(ChatEvent::ContentDelta(text)) => {
                    if let Some(action) = writer.chunk(StreamKind::Output, text) {
                        yield Ok(client_actions(vec![action]));
                    }
                }
                Ok(ChatEvent::ThinkingDelta(text)) => {
                    if let Some(action) = writer.chunk(StreamKind::Reasoning, text) {
                        yield Ok(client_actions(vec![action]));
                    }
                }
                // No tools are offered to the model yet.
                Ok(ChatEvent::ToolCall(_)) => {}
                Ok(ChatEvent::Done(_)) => break,
                Err(error) => {
                    yield Ok(model_error(&config, &error));
                    return;
                }
            }
        }
        yield Ok(finished(stream_finished::Reason::Done(stream_finished::Done {})));
    };

    Ok(Box::pin(stream.take_until(cancellation_rx)))
}

/// The task without a parent, which holds the main conversation.
fn root_task(tasks: &[Task]) -> Option<&Task> {
    tasks.iter().find(|task| {
        task.dependencies
            .as_ref()
            .is_none_or(|dependencies| dependencies.parent_task_id.is_empty())
    })
}

/// The text of each user query in this turn's input.
fn user_queries(input: &api::request::Input) -> Vec<String> {
    use api::request::input::Type;
    use api::request::input::user_inputs::user_input::Input as UserInput;

    #[allow(deprecated)]
    match input.r#type.as_ref() {
        Some(Type::UserInputs(inputs)) => inputs
            .inputs
            .iter()
            .filter_map(|input| match input.input.as_ref() {
                Some(UserInput::UserQuery(query)) => Some(query.query.clone()),
                _ => None,
            })
            .collect(),
        Some(Type::UserQuery(query)) => vec![query.query.clone()],
        _ => Vec::new(),
    }
}

/// Rebuilds the chat history from the root task, merging consecutive output chunks.
fn history_messages(tasks: &[Task]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let Some(root) = root_task(tasks) else {
        return messages;
    };
    for message in &root.messages {
        match message.message.as_ref() {
            Some(MessageKind::UserQuery(query)) => messages.push(ChatMessage::user(&query.query)),
            Some(MessageKind::AgentOutput(output)) => match messages.last_mut() {
                Some(last) if last.role == light_llm::Role::Assistant => {
                    last.content.push_str(&output.text)
                }
                _ => messages.push(ChatMessage::assistant(&output.text)),
            },
            _ => {}
        }
    }
    messages
}

fn system_prompt(context: Option<&api::InputContext>) -> String {
    let mut prompt = String::from(
        "You are an assistant built into the Warp terminal. Answer concisely and accurately. \
         Put commands and code in fenced Markdown code blocks. Reply in the language the user \
         writes in.",
    );
    let Some(context) = context else {
        return prompt;
    };
    if let Some(os) = context
        .operating_system
        .as_ref()
        .filter(|os| !os.platform.is_empty())
    {
        prompt.push_str(&format!("\nOperating system: {}", os.platform));
        if !os.distribution.is_empty() {
            prompt.push_str(&format!(" ({})", os.distribution));
        }
    }
    if let Some(shell) = context
        .shell
        .as_ref()
        .filter(|shell| !shell.name.is_empty())
    {
        prompt.push_str(&format!("\nShell: {} {}", shell.name, shell.version));
    }
    if let Some(directory) = context.directory.as_ref().filter(|dir| !dir.pwd.is_empty()) {
        prompt.push_str(&format!("\nCurrent directory: {}", directory.pwd));
    }
    prompt
}

fn model_error(config: &LocalModelConfig, error: &light_llm::Error) -> ResponseEvent {
    let message = match error {
        light_llm::Error::Http(_) => format!(
            "Could not reach the local model server at {}. Make sure Ollama is running. ({error})",
            config.base_url
        ),
        light_llm::Error::Status { status: 404, .. } => format!(
            "The model '{}' is not installed. Run `ollama pull {}`. ({error})",
            config.model, config.model
        ),
        light_llm::Error::Status { .. }
        | light_llm::Error::Decode(_)
        | light_llm::Error::Server(_) => format!("The local model failed: {error}"),
    };
    log::warn!("Local agent request failed: {error}");
    finished(stream_finished::Reason::InternalError(
        stream_finished::InternalError { message },
    ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Output,
    Reasoning,
}

/// Builds the messages of one turn, streaming text into the message it extends.
struct TurnWriter {
    task_id: String,
    request_id: String,
    open_message: Option<(StreamKind, String)>,
}

impl TurnWriter {
    fn new(task_id: String, request_id: String) -> Self {
        Self {
            task_id,
            request_id,
            open_message: None,
        }
    }

    /// Adds `text` to the open message of the same kind, or starts a new message.
    fn chunk(&mut self, kind: StreamKind, text: String) -> Option<ClientAction> {
        if text.is_empty() {
            return None;
        }
        let payload = match kind {
            StreamKind::Output => MessageKind::AgentOutput(AgentOutput { text }),
            StreamKind::Reasoning => MessageKind::AgentReasoning(AgentReasoning {
                reasoning: text,
                finished_duration: None,
            }),
        };
        if let Some((_, message_id)) = self
            .open_message
            .as_ref()
            .filter(|(open_kind, _)| *open_kind == kind)
        {
            let path = match kind {
                StreamKind::Output => AGENT_OUTPUT_TEXT_PATH,
                StreamKind::Reasoning => AGENT_REASONING_TEXT_PATH,
            };
            return Some(action(client_action::Action::AppendToMessageContent(
                AppendToMessageContent {
                    task_id: self.task_id.clone(),
                    message: Some(Message {
                        id: message_id.clone(),
                        task_id: self.task_id.clone(),
                        request_id: self.request_id.clone(),
                        message: Some(payload),
                        ..Default::default()
                    }),
                    mask: Some(FieldMask {
                        paths: vec![path.to_owned()],
                    }),
                },
            )));
        }
        let message = self.new_message(payload);
        self.open_message = Some((kind, message.id.clone()));
        Some(self.add_messages(vec![message]))
    }

    fn new_message(&self, kind: MessageKind) -> Message {
        Message {
            id: Uuid::new_v4().to_string(),
            task_id: self.task_id.clone(),
            request_id: self.request_id.clone(),
            timestamp: Some(prost_types::Timestamp::from(SystemTime::now())),
            message: Some(kind),
            ..Default::default()
        }
    }

    fn add_messages(&self, messages: Vec<Message>) -> ClientAction {
        action(client_action::Action::AddMessagesToTask(
            AddMessagesToTask {
                task_id: self.task_id.clone(),
                messages,
            },
        ))
    }
}

fn action(action: client_action::Action) -> ClientAction {
    ClientAction {
        action: Some(action),
    }
}

fn client_actions(actions: Vec<ClientAction>) -> ResponseEvent {
    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions { actions },
        )),
    }
}

fn finished(reason: stream_finished::Reason) -> ResponseEvent {
    ResponseEvent {
        r#type: Some(api::response_event::Type::Finished(StreamFinished {
            reason: Some(reason),
            ..Default::default()
        })),
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
