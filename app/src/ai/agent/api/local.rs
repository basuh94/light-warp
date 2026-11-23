//! Serves agent requests with a locally hosted model instead of Warp's servers.
//!
//! The client keeps the whole conversation and resends it with every request, so each turn is
//! translated afresh: the root task's messages become the model's chat history, and the model's
//! streamed reply is turned back into the client actions the Warp server would have sent.

use std::sync::Arc;
use std::time::SystemTime;

use futures_util::StreamExt as _;
use light_llm::{ChatEvent, ChatMessage, ChatOptions, ChatRequest, FunctionCall, Role};
use prost_types::FieldMask;
use serde_json::{Value, json};
use uuid::Uuid;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{AddMessagesToTask, AppendToMessageContent, CreateTask};
use warp_multi_agent_api::message::tool_call::{RunShellCommand, Tool, run_shell_command};
use warp_multi_agent_api::message::tool_call_result::Result as ToolResult;
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, ToolCall, ToolCallResult, UserQuery,
};
use warp_multi_agent_api::response_event::{StreamFinished, StreamInit, stream_finished};
use warp_multi_agent_api::{
    ClientAction, Message, ResponseEvent, RunShellCommandResult, Task, client_action,
    run_shell_command_result,
};

use super::convert_to::convert_input;
use super::{ConvertToAPITypeError, RequestParams, ResponseStream};
use crate::ai::local_models::{self, LocalModelConfig};
use crate::server::server_api::AIApiError;

const AGENT_OUTPUT_TEXT_PATH: &str = "agent_output.text";
const AGENT_REASONING_TEXT_PATH: &str = "agent_reasoning.reasoning";

const RUN_SHELL_COMMAND_TOOL: &str = "run_shell_command";
/// Command output beyond this many characters is cut from the start, keeping the end, which
/// usually holds the result or the error.
const MAX_COMMAND_OUTPUT_CHARS: usize = 8_000;

/// An input of the current turn.
#[derive(Debug, Clone, PartialEq)]
enum TurnInput {
    Query(String),
    CommandResult {
        tool_call_id: String,
        result: RunShellCommandResult,
    },
}

/// Whether agent requests should be served locally.
pub fn is_enabled() -> bool {
    local_models::is_enabled()
}

pub async fn generate_local_agent_output(
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let input = convert_input(params.input)?;
    let local_model = params.local_model;
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

    let Some(config) = local_model else {
        let events: Vec<Result<_, Arc<AIApiError>>> = vec![Ok(init), Ok(not_configured())];
        return Ok(Box::pin(futures::stream::iter(events)));
    };
    // A model picked in the model selector wins over the configured default.
    let requested_model = local_models::model_name_for_llm_id(&params.model)
        .map(str::to_owned)
        .or_else(|| Some(config.default_model.clone()).filter(|model| !model.is_empty()));

    let inputs = turn_inputs(&input);
    if inputs.is_empty() {
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
            inputs
                .iter()
                .map(|turn_input| writer.new_message(echo(turn_input, &input.context)))
                .collect(),
        ),
    );

    let mut messages = vec![ChatMessage::system(system_prompt(input.context.as_ref()))];
    messages.extend(history_messages(&params.tasks));
    messages.extend(inputs.iter().map(chat_message_for_input));
    let client = config.client();

    let stream = async_stream::stream! {
        yield Ok::<_, Arc<AIApiError>>(init);
        yield Ok(client_actions(opening_actions));

        let model = match requested_model {
            Some(model) => model,
            None => match client.list_models().await {
                Ok(installed) => match installed.into_iter().next() {
                    Some(model) => model.name,
                    None => {
                        yield Ok(no_models_installed(&config));
                        return;
                    }
                },
                Err(error) => {
                    yield Ok(model_error(&config, "", &error));
                    return;
                }
            },
        };
        let request = ChatRequest {
            model: model.clone(),
            messages,
            tools: vec![run_shell_command_tool()],
            stream: true,
            options: Some(ChatOptions {
                num_ctx: Some(config.context_window),
            }),
        };
        let mut events = match client.chat_stream(&request).await {
            Ok(events) => events,
            Err(error) => {
                yield Ok(model_error(&config, &model, &error));
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
                Ok(ChatEvent::ToolCall(call)) => match command_from_tool_call(&call.function) {
                    Some(command) => {
                        yield Ok(client_actions(vec![writer.tool_call(command)]));
                    }
                    None => log::warn!(
                        "The local model called an unsupported tool: {}",
                        call.function.name
                    ),
                },
                Ok(ChatEvent::Done(_)) => break,
                Err(error) => {
                    yield Ok(model_error(&config, &model, &error));
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

/// The user queries and command results in this turn's input. Other inputs are not supported by
/// the local model and are dropped.
fn turn_inputs(input: &api::request::Input) -> Vec<TurnInput> {
    use api::request::input::Type;
    use api::request::input::tool_call_result::Result as RequestToolResult;
    use api::request::input::user_inputs::user_input::Input as UserInput;

    #[allow(deprecated)]
    match input.r#type.as_ref() {
        Some(Type::UserInputs(inputs)) => inputs
            .inputs
            .iter()
            .filter_map(|input| match input.input.as_ref() {
                Some(UserInput::UserQuery(query)) => Some(TurnInput::Query(query.query.clone())),
                Some(UserInput::ToolCallResult(tool_result)) => match tool_result.result.as_ref() {
                    Some(RequestToolResult::RunShellCommand(result)) => {
                        Some(TurnInput::CommandResult {
                            tool_call_id: tool_result.tool_call_id.clone(),
                            result: result.clone(),
                        })
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        Some(Type::UserQuery(query)) => vec![TurnInput::Query(query.query.clone())],
        _ => Vec::new(),
    }
}

/// The message that records `turn_input` in the task.
fn echo(turn_input: &TurnInput, context: &Option<api::InputContext>) -> MessageKind {
    match turn_input {
        TurnInput::Query(query) => MessageKind::UserQuery(UserQuery {
            query: query.clone(),
            context: context.clone(),
            ..Default::default()
        }),
        TurnInput::CommandResult {
            tool_call_id,
            result,
        } => MessageKind::ToolCallResult(ToolCallResult {
            tool_call_id: tool_call_id.clone(),
            context: context.clone(),
            result: Some(ToolResult::RunShellCommand(result.clone())),
        }),
    }
}

fn chat_message_for_input(turn_input: &TurnInput) -> ChatMessage {
    match turn_input {
        TurnInput::Query(query) => ChatMessage::user(query),
        TurnInput::CommandResult { result, .. } => {
            ChatMessage::tool_result(RUN_SHELL_COMMAND_TOOL, describe_command_result(result))
        }
    }
}

/// Rebuilds the chat history from the root task, merging consecutive output chunks and
/// attaching each command the agent ran to the assistant message that requested it.
fn history_messages(tasks: &[Task]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let Some(root) = root_task(tasks) else {
        return messages;
    };
    for message in &root.messages {
        match message.message.as_ref() {
            Some(MessageKind::UserQuery(query)) => messages.push(ChatMessage::user(&query.query)),
            Some(MessageKind::AgentOutput(output)) => match messages.last_mut() {
                Some(last) if last.role == Role::Assistant && last.tool_calls.is_empty() => {
                    last.content.push_str(&output.text)
                }
                _ => messages.push(ChatMessage::assistant(&output.text)),
            },
            Some(MessageKind::ToolCall(call)) => {
                let Some(Tool::RunShellCommand(run)) = call.tool.as_ref() else {
                    continue;
                };
                let tool_call = light_llm::ToolCall {
                    function: FunctionCall {
                        name: RUN_SHELL_COMMAND_TOOL.to_owned(),
                        arguments: json!({ "command": run.command }),
                    },
                };
                match messages.last_mut() {
                    Some(last) if last.role == Role::Assistant => last.tool_calls.push(tool_call),
                    _ => messages.push(ChatMessage {
                        tool_calls: vec![tool_call],
                        ..ChatMessage::assistant("")
                    }),
                }
            }
            Some(MessageKind::ToolCallResult(result)) => {
                if let Some(ToolResult::RunShellCommand(result)) = result.result.as_ref() {
                    messages.push(ChatMessage::tool_result(
                        RUN_SHELL_COMMAND_TOOL,
                        describe_command_result(result),
                    ));
                }
            }
            _ => {}
        }
    }
    messages
}

fn run_shell_command_tool() -> light_llm::Tool {
    light_llm::Tool::function(
        RUN_SHELL_COMMAND_TOOL,
        "Runs a command in the user's terminal and returns its output and exit code. The user \
         reviews and approves each command before it runs.",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to run, written for the user's shell.",
                },
            },
            "required": ["command"],
        }),
    )
}

/// The command a `run_shell_command` call asks to run, if the call is well formed.
fn command_from_tool_call(call: &FunctionCall) -> Option<String> {
    if call.name != RUN_SHELL_COMMAND_TOOL {
        return None;
    }
    // Some models send the arguments as a JSON-encoded string rather than an object.
    let decoded;
    let arguments = match call.arguments.as_str() {
        Some(encoded) => {
            decoded = serde_json::from_str::<Value>(encoded).ok()?;
            &decoded
        }
        None => &call.arguments,
    };
    arguments
        .get("command")?
        .as_str()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
}

/// Describes a command's outcome for the model.
fn describe_command_result(result: &RunShellCommandResult) -> String {
    use run_shell_command_result::Result as Outcome;

    match result.result.as_ref() {
        Some(Outcome::CommandFinished(finished)) => format!(
            "Exit code: {}\n{}",
            finished.exit_code,
            truncate_command_output(&finished.output)
        ),
        Some(Outcome::LongRunningCommandSnapshot(_)) => {
            "The command is still running in the user's terminal.".to_owned()
        }
        Some(Outcome::PermissionDenied(_)) => {
            "The command was not run because it is on the user's deny list.".to_owned()
        }
        Some(Outcome::TerminalBusy(_)) => {
            "The command was not run because the terminal is busy with another command.".to_owned()
        }
        #[allow(deprecated)]
        None => format!(
            "Exit code: {}\n{}",
            result.exit_code,
            truncate_command_output(&result.output)
        ),
    }
}

fn truncate_command_output(output: &str) -> String {
    let char_count = output.chars().count();
    if char_count <= MAX_COMMAND_OUTPUT_CHARS {
        return output.to_owned();
    }
    let start = output
        .char_indices()
        .nth(char_count - MAX_COMMAND_OUTPUT_CHARS)
        .map(|(index, _)| index)
        .unwrap_or_default();
    format!(
        "[Output truncated: showing the last {MAX_COMMAND_OUTPUT_CHARS} of {char_count} characters]\n{}",
        &output[start..]
    )
}

fn system_prompt(context: Option<&api::InputContext>) -> String {
    let mut prompt = String::from(
        "You are an assistant built into the Warp terminal. Answer concisely and accurately. \
         Put commands and code in fenced Markdown code blocks. Reply in the language the user \
         writes in.\n\
         You can run commands in the user's terminal with the run_shell_command tool. Use it \
         when the task needs information from the user's machine or asks you to do something \
         there; answer directly when it does not. The user approves every command before it \
         runs. Run one command at a time, prefer commands that only read, and never run \
         destructive commands unless the user asked for them. After a command runs you get its \
         output; use it to continue the task or to answer.",
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

fn model_error(config: &LocalModelConfig, model: &str, error: &light_llm::Error) -> ResponseEvent {
    let message = match error {
        light_llm::Error::Http(_) => format!(
            "Could not reach the local model server at {}. Make sure Ollama is running. ({error})",
            config.base_url
        ),
        light_llm::Error::Status { status: 404, .. } => format!(
            "The model '{}' is not installed. Run `ollama pull {}`. ({error})",
            model, model
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

fn no_models_installed(config: &LocalModelConfig) -> ResponseEvent {
    finished(stream_finished::Reason::InternalError(
        stream_finished::InternalError {
            message: format!(
                "The local model server at {} has no models installed. Install one, for \
                 example with `ollama pull qwen3:8b`.",
                config.base_url
            ),
        },
    ))
}

/// Reported as an invalid API key, which the client renders with a button that opens the
/// settings; in builds that serve agent requests locally, that error means no model server is
/// configured.
fn not_configured() -> ResponseEvent {
    finished(stream_finished::Reason::InvalidApiKey(
        stream_finished::InvalidApiKey {
            provider: api::LlmProvider::Unknown as i32,
            model_name: String::new(),
        },
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

    /// Asks the client to run `command`. The command is marked risky and not read-only, which
    /// makes the client ask the user for approval unless their allowlist covers it.
    #[allow(deprecated)]
    fn tool_call(&mut self, command: String) -> ClientAction {
        self.open_message = None;
        let message = self.new_message(MessageKind::ToolCall(ToolCall {
            tool_call_id: Uuid::new_v4().to_string(),
            tool: Some(Tool::RunShellCommand(RunShellCommand {
                command,
                is_read_only: false,
                is_risky: true,
                wait_until_complete_value: Some(
                    run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(true),
                ),
                ..Default::default()
            })),
        }));
        self.add_messages(vec![message])
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
