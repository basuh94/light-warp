use light_llm::ChatMessage;
use warp_multi_agent_api as api;
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, UserQuery,
};
use warp_multi_agent_api::{Message, Task, client_action, task};

use super::{StreamKind, TurnWriter, history_messages, root_task, system_prompt, user_queries};

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
fn user_queries_reads_every_query_in_the_turn() {
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

    assert_eq!(user_queries(&input), vec!["uno", "dos"]);
}

#[test]
fn inputs_that_are_not_user_turns_have_no_queries() {
    let input = api::request::Input {
        context: None,
        r#type: Some(api::request::input::Type::ResumeConversation(
            api::request::input::ResumeConversation {},
        )),
    };

    assert!(user_queries(&input).is_empty());
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
