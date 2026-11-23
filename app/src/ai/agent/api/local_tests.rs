use light_llm::ChatMessage;
use warp_multi_agent_api as api;
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, ToolCall, UserQuery,
};
use warp_multi_agent_api::{Message, Task, client_action, task};

use light_llm::FunctionCall;
use serde_json::json;
use warp_multi_agent_api::message::ToolCallResult as ToolCallResultMessage;
use warp_multi_agent_api::message::tool_call::{RunShellCommand, Tool};
use warp_multi_agent_api::message::tool_call_result::Result as ToolResult;
use warp_multi_agent_api::{RunShellCommandResult, ShellCommandFinished, run_shell_command_result};

use super::{
    StreamKind, TurnInput, TurnWriter, command_from_tool_call, describe_command_result,
    history_messages, root_task, system_prompt, turn_inputs,
};

fn message(kind: MessageKind) -> Message {
    Message {
        message: Some(kind),
        ..Default::default()
    }
}

fn root_with(messages: Vec<Message>) -> Task {
    Task {
        id: "root".to_owned(),
        messages,
        ..Default::default()
    }
}

#[test]
fn root_task_skips_subtasks() {
    let subtask = Task {
        id: "child".to_owned(),
        dependencies: Some(task::Dependencies {
            parent_task_id: "root".to_owned(),
        }),
        ..Default::default()
    };
    let tasks = vec![subtask, root_with(vec![])];

    assert_eq!(root_task(&tasks).map(|task| task.id.as_str()), Some("root"));
}

#[test]
fn history_merges_streamed_output_and_drops_reasoning() {
    let tasks = vec![root_with(vec![
        message(MessageKind::UserQuery(UserQuery {
            query: "hola".to_owned(),
            ..Default::default()
        })),
        message(MessageKind::AgentReasoning(AgentReasoning {
            reasoning: "pensando".to_owned(),
            finished_duration: None,
        })),
        message(MessageKind::AgentOutput(AgentOutput {
            text: "Ho".to_owned(),
        })),
        message(MessageKind::AgentOutput(AgentOutput {
            text: "la".to_owned(),
        })),
        message(MessageKind::UserQuery(UserQuery {
            query: "adios".to_owned(),
            ..Default::default()
        })),
    ])];

    assert_eq!(
        history_messages(&tasks),
        vec![
            ChatMessage::user("hola"),
            ChatMessage::assistant("Hola"),
            ChatMessage::user("adios"),
        ]
    );
}

#[test]
fn turn_inputs_reads_every_query_in_the_turn() {
    use api::request::input::user_inputs::UserInput;
    use api::request::input::user_inputs::user_input::Input;
    use api::request::input::{Type, UserInputs, UserQuery};

    let query = |text: &str| UserInput {
        input: Some(Input::UserQuery(UserQuery {
            query: text.to_owned(),
            ..Default::default()
        })),
    };
    let input = api::request::Input {
        context: None,
        r#type: Some(Type::UserInputs(UserInputs {
            inputs: vec![query("uno"), query("dos")],
        })),
    };

    assert_eq!(
        turn_inputs(&input),
        vec![
            TurnInput::Query("uno".to_owned()),
            TurnInput::Query("dos".to_owned()),
        ]
    );
}

#[test]
fn inputs_that_are_not_user_turns_are_dropped() {
    let input = api::request::Input {
        context: None,
        r#type: Some(api::request::input::Type::ResumeConversation(
            api::request::input::ResumeConversation {},
        )),
    };

    assert!(turn_inputs(&input).is_empty());
}

#[test]
fn system_prompt_describes_the_environment() {
    let context = api::InputContext {
        directory: Some(api::input_context::Directory {
            pwd: "C:\\proyecto".to_owned(),
            ..Default::default()
        }),
        operating_system: Some(api::input_context::OperatingSystem {
            platform: "Windows".to_owned(),
            distribution: String::new(),
        }),
        shell: Some(api::input_context::Shell {
            name: "pwsh".to_owned(),
            version: "5.1".to_owned(),
        }),
        ..Default::default()
    };

    let prompt = system_prompt(Some(&context));

    assert!(prompt.contains("Operating system: Windows"));
    assert!(prompt.contains("Shell: pwsh 5.1"));
    assert!(prompt.contains("Current directory: C:\\proyecto"));
}

#[test]
fn writer_appends_to_the_open_message_of_the_same_kind() {
    let mut writer = TurnWriter::new("root".to_owned(), "request".to_owned());

    let first = writer.chunk(StreamKind::Output, "Ho".to_owned()).unwrap();
    let second = writer.chunk(StreamKind::Output, "la".to_owned()).unwrap();
    let reasoning = writer
        .chunk(StreamKind::Reasoning, "hmm".to_owned())
        .unwrap();
    assert!(writer.chunk(StreamKind::Output, String::new()).is_none());

    let Some(client_action::Action::AddMessagesToTask(added)) = first.action else {
        panic!("the first chunk should add a message");
    };
    assert_eq!(added.task_id, "root");
    let first_message = &added.messages[0];
    assert!(matches!(
        &first_message.message,
        Some(MessageKind::AgentOutput(output)) if output.text == "Ho"
    ));

    let Some(client_action::Action::AppendToMessageContent(appended)) = second.action else {
        panic!("the second chunk should extend the open message");
    };
    let appended_message = appended.message.unwrap();
    assert_eq!(appended_message.id, first_message.id);
    assert!(matches!(
        appended_message.message,
        Some(MessageKind::AgentOutput(output)) if output.text == "la"
    ));
    assert_eq!(appended.mask.unwrap().paths, vec!["agent_output.text"]);

    assert!(matches!(
        reasoning.action,
        Some(client_action::Action::AddMessagesToTask(_))
    ));
}

fn finished_command(output: &str, exit_code: i32) -> RunShellCommandResult {
    RunShellCommandResult {
        command: "ls".to_owned(),
        result: Some(run_shell_command_result::Result::CommandFinished(
            ShellCommandFinished {
                output: output.to_owned(),
                exit_code,
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

#[test]
fn turn_inputs_reads_command_results() {
    use api::request::input::user_inputs::UserInput;
    use api::request::input::user_inputs::user_input::Input;
    use api::request::input::{ToolCallResult, Type, UserInputs, tool_call_result};

    let input = api::request::Input {
        context: None,
        r#type: Some(Type::UserInputs(UserInputs {
            inputs: vec![UserInput {
                input: Some(Input::ToolCallResult(ToolCallResult {
                    tool_call_id: "call-1".to_owned(),
                    result: Some(tool_call_result::Result::RunShellCommand(finished_command(
                        "a.txt", 0,
                    ))),
                    ..Default::default()
                })),
            }],
        })),
    };

    assert_eq!(
        turn_inputs(&input),
        vec![TurnInput::CommandResult {
            tool_call_id: "call-1".to_owned(),
            result: finished_command("a.txt", 0),
        }]
    );
}

#[test]
fn history_attaches_commands_to_the_assistant_and_results_as_tool_messages() {
    let tasks = vec![root_with(vec![
        message(MessageKind::UserQuery(UserQuery {
            query: "¿qué hay aquí?".to_owned(),
            ..Default::default()
        })),
        message(MessageKind::AgentOutput(AgentOutput {
            text: "Miro la carpeta.".to_owned(),
        })),
        message(MessageKind::ToolCall(ToolCall {
            tool_call_id: "call-1".to_owned(),
            tool: Some(Tool::RunShellCommand(RunShellCommand {
                command: "ls".to_owned(),
                ..Default::default()
            })),
        })),
        message(MessageKind::ToolCallResult(ToolCallResultMessage {
            tool_call_id: "call-1".to_owned(),
            context: None,
            result: Some(ToolResult::RunShellCommand(finished_command("a.txt", 0))),
        })),
        message(MessageKind::AgentOutput(AgentOutput {
            text: "Hay un fichero.".to_owned(),
        })),
    ])];

    let history = history_messages(&tasks);

    assert_eq!(history.len(), 4);
    assert_eq!(history[1].content, "Miro la carpeta.");
    assert_eq!(
        history[1].tool_calls,
        vec![light_llm::ToolCall {
            function: FunctionCall {
                name: "run_shell_command".to_owned(),
                arguments: json!({"command": "ls"}),
            },
        }]
    );
    assert_eq!(
        history[2],
        ChatMessage::tool_result("run_shell_command", "Exit code: 0\na.txt")
    );
    assert_eq!(history[3], ChatMessage::assistant("Hay un fichero."));
}

#[test]
fn command_from_tool_call_accepts_object_and_encoded_arguments() {
    let call = |name: &str, arguments: serde_json::Value| FunctionCall {
        name: name.to_owned(),
        arguments,
    };

    assert_eq!(
        command_from_tool_call(&call("run_shell_command", json!({"command": " ls -la "}))),
        Some("ls -la".to_owned())
    );
    assert_eq!(
        command_from_tool_call(&call("run_shell_command", json!("{\"command\":\"dir\"}"))),
        Some("dir".to_owned())
    );
    assert_eq!(
        command_from_tool_call(&call("run_shell_command", json!({"command": "  "}))),
        None
    );
    assert_eq!(
        command_from_tool_call(&call("delete_everything", json!({"command": "ls"}))),
        None
    );
}

#[test]
fn long_command_output_keeps_its_end() {
    let output = format!("{}FIN", "x".repeat(10_000));

    let described = describe_command_result(&finished_command(&output, 1));

    assert!(described.starts_with("Exit code: 1\n[Output truncated"));
    assert!(described.ends_with("FIN"));
    assert!(described.len() < 8_200);
}

#[test]
#[allow(deprecated)]
fn tool_calls_always_require_approval() {
    let mut writer = TurnWriter::new("root".to_owned(), "request".to_owned());
    writer.chunk(StreamKind::Output, "Voy a mirar.".to_owned());

    let action = writer.tool_call("ls".to_owned());

    let Some(client_action::Action::AddMessagesToTask(added)) = action.action else {
        panic!("a tool call should add a message");
    };
    let Some(MessageKind::ToolCall(call)) = &added.messages[0].message else {
        panic!("the message should be a tool call");
    };
    let Some(Tool::RunShellCommand(run)) = &call.tool else {
        panic!("the tool call should run a command");
    };
    assert_eq!(run.command, "ls");
    assert!(run.is_risky);
    assert!(!run.is_read_only);
    // Text after the call starts a new message instead of extending the one before it.
    assert!(matches!(
        writer
            .chunk(StreamKind::Output, "Hecho.".to_owned())
            .and_then(|action| action.action),
        Some(client_action::Action::AddMessagesToTask(_))
    ));
}
