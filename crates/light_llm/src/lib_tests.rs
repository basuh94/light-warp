use futures::StreamExt as _;
use futures::executor::block_on;
use serde_json::json;

use super::*;

fn client_for(server: &mockito::Server) -> OllamaClient {
    OllamaClient::with_http_client(server.url(), http_client::Client::new_external_for_test())
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "qwen3:8b".to_owned(),
        messages: vec![ChatMessage::system("sys"), ChatMessage::user("hola")],
        tools: vec![],
        stream: true,
        options: Some(ChatOptions {
            num_ctx: Some(16384),
        }),
    }
}

#[test]
fn chat_request_serializes_to_the_native_ollama_shape() {
    let mut request = request();
    request.tools = vec![Tool::function(
        "run_shell_command",
        "Runs a command",
        json!({"type": "object"}),
    )];
    request
        .messages
        .push(ChatMessage::tool_result("run_shell_command", "ok"));

    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({
            "model": "qwen3:8b",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hola"},
                {"role": "tool", "content": "ok", "tool_name": "run_shell_command"},
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "run_shell_command",
                    "description": "Runs a command",
                    "parameters": {"type": "object"},
                },
            }],
            "stream": true,
            "options": {"num_ctx": 16384},
        })
    );
}

#[test]
fn chat_stream_yields_content_tool_calls_and_usage() {
    let mut server = mockito::Server::new();
    let body = [
        json!({"message": {"role": "assistant", "content": "", "thinking": "pienso"}, "done": false}),
        json!({"message": {"role": "assistant", "content": "Ho"}, "done": false}),
        json!({"message": {"role": "assistant", "content": "la"}, "done": false}),
        json!({"message": {"role": "assistant", "content": "", "tool_calls": [
            {"function": {"name": "run_shell_command", "arguments": {"command": "ls"}}}
        ]}, "done": false}),
        json!({"message": {"role": "assistant", "content": ""}, "done": true,
               "done_reason": "stop", "prompt_eval_count": 12, "eval_count": 3}),
    ]
    .iter()
    .map(|line| format!("{line}\n"))
    .collect::<String>();
    let mock = server
        .mock("POST", "/api/chat")
        .match_body(mockito::Matcher::PartialJson(json!({
            "model": "qwen3:8b",
            "stream": true,
            "options": {"num_ctx": 16384},
        })))
        .with_header("content-type", "application/x-ndjson")
        .with_body(body)
        .create();

    let events: Vec<_> = block_on(async {
        client_for(&server)
            .chat_stream(&request())
            .await
            .unwrap()
            .collect()
            .await
    });

    mock.assert();
    let events: Vec<_> = events.into_iter().map(Result::unwrap).collect();
    assert_eq!(
        events,
        vec![
            ChatEvent::ThinkingDelta("pienso".to_owned()),
            ChatEvent::ContentDelta("Ho".to_owned()),
            ChatEvent::ContentDelta("la".to_owned()),
            ChatEvent::ToolCall(ToolCall {
                function: FunctionCall {
                    name: "run_shell_command".to_owned(),
                    arguments: json!({"command": "ls"}),
                },
            }),
            ChatEvent::Done(Usage {
                prompt_tokens: 12,
                output_tokens: 3,
                done_reason: Some("stop".to_owned()),
            }),
        ]
    );
}

#[test]
fn an_error_line_ends_the_stream_with_a_server_error() {
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/api/chat")
        .with_body("{\"message\":{\"role\":\"assistant\",\"content\":\"a\"},\"done\":false}\n{\"error\":\"model crashed\"}\n")
        .create();

    let events: Vec<_> = block_on(async {
        client_for(&server)
            .chat_stream(&request())
            .await
            .unwrap()
            .collect()
            .await
    });

    assert_eq!(events.len(), 2);
    assert!(matches!(&events[0], Ok(ChatEvent::ContentDelta(text)) if text == "a"));
    assert!(matches!(&events[1], Err(Error::Server(message)) if message == "model crashed"));
}

#[test]
fn http_errors_carry_the_server_message() {
    let mut server = mockito::Server::new();
    server
        .mock("POST", "/api/chat")
        .with_status(404)
        .with_body("{\"error\":\"model 'nope' not found\"}")
        .create();

    let result = block_on(client_for(&server).chat_stream(&request()));

    assert!(matches!(
        result,
        Err(Error::Status { status: 404, body }) if body == "model 'nope' not found"
    ));
}

#[test]
fn list_models_reads_installed_models() {
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/api/tags")
        .with_body(
            json!({"models": [
                {"name": "qwen3:8b", "size": 5_200_000_000_u64, "details": {}},
                {"name": "qwen3:4b", "size": 2_600_000_000_u64},
            ]})
            .to_string(),
        )
        .create();

    let models = block_on(client_for(&server).list_models()).unwrap();

    assert_eq!(
        models,
        vec![
            ModelInfo {
                name: "qwen3:8b".to_owned(),
                size: 5_200_000_000,
            },
            ModelInfo {
                name: "qwen3:4b".to_owned(),
                size: 2_600_000_000,
            },
        ]
    );
}
