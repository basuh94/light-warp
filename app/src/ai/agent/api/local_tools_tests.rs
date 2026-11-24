use light_llm::FunctionCall;
use serde_json::{Value, json};
use warp_multi_agent_api::message::tool_call::Tool;
use warp_multi_agent_api::{
    ApplyFileDiffsResult, FileGlobV2Result, GrepResult, RunShellCommandResult,
    ShellCommandFinished, apply_file_diffs_result, file_glob_v2_result, grep_result,
    run_shell_command_result,
};

use super::{FileToRead, LocalTool, ToolOutcome, definitions};

fn call(name: &str, arguments: Value) -> FunctionCall {
    FunctionCall {
        name: name.to_owned(),
        arguments,
    }
}

#[test]
fn every_defined_tool_is_parsed() {
    for definition in definitions() {
        let definition = serde_json::to_value(definition).unwrap();
        let name = definition["function"]["name"].as_str().unwrap().to_owned();
        let tool = LocalTool::from_function_call(&call(
            &name,
            json!({
                "command": "ls",
                "files": [{"path": "a.txt"}],
                "queries": ["fn main"],
                "patterns": ["*.rs"],
                "path": "src/main.rs",
                "search": "old",
                "replace": "new",
                "content": "text",
            }),
        ));
        assert_eq!(tool.map(|tool| tool.name()), Some(name.as_str()));
    }
}

#[test]
fn run_shell_command_accepts_object_and_encoded_arguments() {
    assert_eq!(
        LocalTool::from_function_call(&call("run_shell_command", json!({"command": " ls -la "}))),
        Some(LocalTool::RunShellCommand {
            command: "ls -la".to_owned()
        })
    );
    assert_eq!(
        LocalTool::from_function_call(&call(
            "run_shell_command",
            json!("{\"command\":\"dir\"}")
        )),
        Some(LocalTool::RunShellCommand {
            command: "dir".to_owned()
        })
    );
    assert_eq!(
        LocalTool::from_function_call(&call("run_shell_command", json!({"command": "  "}))),
        None
    );
    assert_eq!(
        LocalTool::from_function_call(&call("delete_everything", json!({"command": "ls"}))),
        None
    );
}

#[test]
fn read_files_accepts_paths_and_line_ranges() {
    let tool = LocalTool::from_function_call(&call(
        "read_files",
        json!({"files": [
            "a.txt",
            {"path": "b.txt", "start_line": 10, "end_line": 20},
            {"path": "c.txt", "start_line": 5},
            {"path": "  "},
        ]}),
    ));

    assert_eq!(
        tool,
        Some(LocalTool::ReadFiles {
            files: vec![
                FileToRead {
                    path: "a.txt".to_owned(),
                    lines: None,
                },
                FileToRead {
                    path: "b.txt".to_owned(),
                    lines: Some((10, 20)),
                },
                FileToRead {
                    path: "c.txt".to_owned(),
                    lines: Some((5, 305)),
                },
            ],
        })
    );
}

#[test]
fn search_tools_accept_a_single_string() {
    assert_eq!(
        LocalTool::from_function_call(&call("grep", json!({"query": "TODO"}))),
        Some(LocalTool::Grep {
            queries: vec!["TODO".to_owned()],
            path: String::new(),
        })
    );
    assert_eq!(
        LocalTool::from_function_call(&call("find_files", json!({"patterns": "*.toml"}))),
        Some(LocalTool::FindFiles {
            patterns: vec!["*.toml".to_owned()],
            path: String::new(),
        })
    );
}

#[test]
fn edits_need_a_path_and_text_to_replace() {
    assert_eq!(
        LocalTool::from_function_call(&call("edit_file", json!({"path": "a.rs", "search": ""}))),
        None
    );
    assert_eq!(
        LocalTool::from_function_call(&call("create_file", json!({"content": "x"}))),
        None
    );
}

#[test]
fn tools_round_trip_through_the_recorded_tool_call() {
    let tools = [
        LocalTool::RunShellCommand {
            command: "ls".to_owned(),
        },
        LocalTool::ReadFiles {
            files: vec![FileToRead {
                path: "a.txt".to_owned(),
                lines: Some((1, 5)),
            }],
        },
        LocalTool::Grep {
            queries: vec!["x".to_owned()],
            path: "src".to_owned(),
        },
        LocalTool::FindFiles {
            patterns: vec!["*.rs".to_owned()],
            path: "src".to_owned(),
        },
        LocalTool::EditFile {
            path: "a.rs".to_owned(),
            search: "old".to_owned(),
            replace: "new".to_owned(),
        },
        LocalTool::CreateFile {
            path: "b.rs".to_owned(),
            content: "text".to_owned(),
        },
    ];

    for tool in tools {
        let api_tool = tool.clone().into_api_tool();
        assert_eq!(LocalTool::from_api_tool(&api_tool), Some(tool.clone()));
        assert_eq!(
            LocalTool::from_function_call(&tool.to_function_call()),
            Some(tool)
        );
    }
}

#[test]
#[allow(deprecated)]
fn commands_are_marked_for_approval() {
    let Tool::RunShellCommand(run) = (LocalTool::RunShellCommand {
        command: "ls".to_owned(),
    })
    .into_api_tool() else {
        panic!("expected a shell command");
    };
    assert!(run.is_risky);
    assert!(!run.is_read_only);
}

#[test]
fn search_tools_default_to_the_current_directory() {
    let Tool::Grep(grep) = (LocalTool::Grep {
        queries: vec!["x".to_owned()],
        path: String::new(),
    })
    .into_api_tool() else {
        panic!("expected a grep");
    };
    assert_eq!(grep.path, ".");
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
fn long_command_output_keeps_its_end() {
    let output = format!("{}FIN", "x".repeat(10_000));

    let described = ToolOutcome::Command(finished_command(&output, 1)).describe();

    assert!(described.starts_with("Exit code: 1\n[Output truncated"));
    assert!(described.ends_with("FIN"));
    assert!(described.len() < 8_200);
}

#[test]
fn grep_results_list_files_and_lines() {
    use grep_result::success::GrepFileMatch;
    use grep_result::success::grep_file_match::GrepLineMatch;

    let outcome = ToolOutcome::Grep(GrepResult {
        result: Some(grep_result::Result::Success(grep_result::Success {
            matched_files: vec![GrepFileMatch {
                file_path: "src/main.rs".to_owned(),
                matched_lines: vec![
                    GrepLineMatch { line_number: 3 },
                    GrepLineMatch { line_number: 12 },
                ],
            }],
        })),
    });

    assert_eq!(outcome.describe(), "src/main.rs: lines 3, 12");
}

#[test]
fn empty_searches_say_so() {
    let grep = ToolOutcome::Grep(GrepResult {
        result: Some(grep_result::Result::Success(grep_result::Success {
            matched_files: vec![],
        })),
    });
    let glob = ToolOutcome::FindFiles(FileGlobV2Result {
        result: Some(file_glob_v2_result::Result::Success(
            file_glob_v2_result::Success::default(),
        )),
    });

    assert_eq!(grep.describe(), "No matches.");
    assert_eq!(glob.describe(), "No files found.");
}

#[test]
fn failed_edits_ask_the_model_to_reread_the_file() {
    let outcome = ToolOutcome::FileEdits(ApplyFileDiffsResult {
        result: Some(apply_file_diffs_result::Result::Error(
            apply_file_diffs_result::Error {
                message: "search text not found".to_owned(),
            },
        )),
    });

    let described = outcome.describe();

    assert!(described.contains("search text not found"));
    assert!(described.contains("Read the file again"));
}

#[test]
fn running_commands_report_their_output_so_far() {
    let outcome = ToolOutcome::Command(RunShellCommandResult {
        command: "dir /s".to_owned(),
        result: Some(run_shell_command_result::Result::LongRunningCommandSnapshot(
            warp_multi_agent_api::LongRunningShellCommandSnapshot {
                output: "C:\\proyecto\\Cargo.toml".to_owned(),
                ..Default::default()
            },
        )),
        ..Default::default()
    });

    let described = outcome.describe();

    assert!(described.contains("still running"));
    assert!(described.ends_with("C:\\proyecto\\Cargo.toml"));
}
