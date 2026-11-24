//! The tools the local model can call, and their translation to and from the tool calls the
//! client executes.
//!
//! The client runs every tool and applies the user's permissions to it, so the local model gets
//! the same approval flow as the Warp server's agent: commands and file edits are reviewed by the
//! user, while reads follow the user's read permissions.

use serde_json::{Value, json};
use warp_multi_agent_api::message::tool_call::{
    ApplyFileDiffs, FileGlobV2, Grep, ReadFiles, RunShellCommand, Tool, apply_file_diffs,
    read_files, run_shell_command,
};
use warp_multi_agent_api::message::tool_call_result::Result as MessageResult;
use warp_multi_agent_api::request::input::tool_call_result::Result as RequestResult;
use warp_multi_agent_api::{
    ApplyFileDiffsResult, FileContent, FileContentLineRange, FileGlobV2Result, GrepResult,
    ReadFilesResult, RunShellCommandResult, any_file_content, apply_file_diffs_result,
    file_glob_v2_result, grep_result, read_files_result, run_shell_command_result,
};

const RUN_SHELL_COMMAND: &str = "run_shell_command";
const READ_FILES: &str = "read_files";
const GREP: &str = "grep";
const FIND_FILES: &str = "find_files";
const EDIT_FILE: &str = "edit_file";
const CREATE_FILE: &str = "create_file";

/// Command output beyond this many characters is cut from the start, keeping the end, which
/// usually holds the result or the error.
const MAX_COMMAND_OUTPUT_CHARS: usize = 8_000;
/// File contents beyond this many characters are cut from the end; the model can read the rest
/// by line range.
const MAX_FILE_CHARS: usize = 12_000;
const MAX_LISTED_PATHS: usize = 200;
/// Lines read when the model gives a start line but no end line.
const LINES_READ_FROM_START: u32 = 300;

/// A tool call made by the local model.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum LocalTool {
    RunShellCommand {
        command: String,
    },
    ReadFiles {
        files: Vec<FileToRead>,
    },
    Grep {
        queries: Vec<String>,
        path: String,
    },
    FindFiles {
        patterns: Vec<String>,
        path: String,
    },
    EditFile {
        path: String,
        search: String,
        replace: String,
    },
    CreateFile {
        path: String,
        content: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct FileToRead {
    pub path: String,
    /// 1-based, inclusive line range; the whole file when absent.
    pub lines: Option<(u32, u32)>,
}

/// The tool definitions sent to the model.
pub(super) fn definitions() -> Vec<light_llm::Tool> {
    vec![
        light_llm::Tool::function(
            RUN_SHELL_COMMAND,
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
        ),
        light_llm::Tool::function(
            READ_FILES,
            "Reads the contents of one or more text files. Prefer this over shell commands to look \
             at files.",
            json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": {
                                    "type": "string",
                                    "description": "Absolute path, or relative to the current directory.",
                                },
                                "start_line": {
                                    "type": "integer",
                                    "description": "First line to read (1-based). Omit to read the whole file.",
                                },
                                "end_line": {
                                    "type": "integer",
                                    "description": "Last line to read (inclusive).",
                                },
                            },
                            "required": ["path"],
                        },
                    },
                },
                "required": ["files"],
            }),
        ),
        light_llm::Tool::function(
            GREP,
            "Searches file contents for regular expressions and returns the matching files and \
             line numbers.",
            json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Regular expressions to search for.",
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search, relative to the current directory. Defaults to the current directory.",
                    },
                },
                "required": ["queries"],
            }),
        ),
        light_llm::Tool::function(
            FIND_FILES,
            "Finds files whose names match glob patterns such as *.rs or Cargo.toml.",
            json!({
                "type": "object",
                "properties": {
                    "patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "File name patterns. Supports ?, * and [].",
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search, relative to the current directory. Defaults to the current directory.",
                    },
                },
                "required": ["patterns"],
            }),
        ),
        light_llm::Tool::function(
            EDIT_FILE,
            "Edits a file by replacing one exact piece of its text. The user reviews the change \
             before it is applied. Read the file first so that `search` matches it exactly.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file to edit." },
                    "search": {
                        "type": "string",
                        "description": "The exact text to replace, including whitespace. It must appear once in the file.",
                    },
                    "replace": { "type": "string", "description": "The text that replaces `search`." },
                },
                "required": ["path", "search", "replace"],
            }),
        ),
        light_llm::Tool::function(
            CREATE_FILE,
            "Creates a new file with the given content. The user reviews the file before it is \
             created.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "The file to create." },
                    "content": { "type": "string", "description": "The full content of the file." },
                },
                "required": ["path", "content"],
            }),
        ),
    ]
}

impl LocalTool {
    /// Reads a tool call made by the model. Returns `None` for unknown tools and calls that miss
    /// a required argument.
    pub(super) fn from_function_call(call: &light_llm::FunctionCall) -> Option<Self> {
        // Some models send the arguments as a JSON-encoded string rather than an object.
        let decoded;
        let arguments = match call.arguments.as_str() {
            Some(encoded) => {
                decoded = serde_json::from_str::<Value>(encoded).ok()?;
                &decoded
            }
            None => &call.arguments,
        };
        let text = |name: &str| arguments.get(name).and_then(Value::as_str);
        let required_text = |name: &str| {
            text(name)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let optional_path = || text("path").map(str::trim).unwrap_or_default().to_owned();

        match call.name.as_str() {
            RUN_SHELL_COMMAND => Some(Self::RunShellCommand {
                command: required_text("command")?,
            }),
            READ_FILES => {
                let files: Vec<FileToRead> = string_or_object_list(arguments.get("files"))
                    .into_iter()
                    .filter_map(|file| file_to_read(&file))
                    .collect();
                (!files.is_empty()).then_some(Self::ReadFiles { files })
            }
            GREP => {
                let queries = string_list(arguments.get("queries").or(arguments.get("query")));
                (!queries.is_empty()).then(|| Self::Grep {
                    queries,
                    path: optional_path(),
                })
            }
            FIND_FILES => {
                let patterns = string_list(arguments.get("patterns").or(arguments.get("pattern")));
                (!patterns.is_empty()).then(|| Self::FindFiles {
                    patterns,
                    path: optional_path(),
                })
            }
            EDIT_FILE => {
                let search = text("search").filter(|search| !search.is_empty())?;
                Some(Self::EditFile {
                    path: required_text("path")?,
                    search: search.to_owned(),
                    replace: text("replace").unwrap_or_default().to_owned(),
                })
            }
            CREATE_FILE => Some(Self::CreateFile {
                path: required_text("path")?,
                content: text("content").unwrap_or_default().to_owned(),
            }),
            _ => None,
        }
    }

    /// Reads a tool call recorded in the conversation, if it is one the local model can make.
    pub(super) fn from_api_tool(tool: &Tool) -> Option<Self> {
        match tool {
            Tool::RunShellCommand(run) => Some(Self::RunShellCommand {
                command: run.command.clone(),
            }),
            Tool::ReadFiles(read) => Some(Self::ReadFiles {
                files: read
                    .files
                    .iter()
                    .map(|file| FileToRead {
                        path: file.name.clone(),
                        lines: file.line_ranges.first().map(|range| (range.start, range.end)),
                    })
                    .collect(),
            }),
            Tool::Grep(grep) => Some(Self::Grep {
                queries: grep.queries.clone(),
                path: grep.path.clone(),
            }),
            Tool::FileGlobV2(glob) => Some(Self::FindFiles {
                patterns: glob.patterns.clone(),
                path: glob.search_dir.clone(),
            }),
            Tool::ApplyFileDiffs(apply) => {
                if let [diff] = apply.diffs.as_slice() {
                    Some(Self::EditFile {
                        path: diff.file_path.clone(),
                        search: diff.search.clone(),
                        replace: diff.replace.clone(),
                    })
                } else if let [new_file] = apply.new_files.as_slice() {
                    Some(Self::CreateFile {
                        path: new_file.file_path.clone(),
                        content: new_file.content.clone(),
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub(super) fn name(&self) -> &'static str {
        match self {
            Self::RunShellCommand { .. } => RUN_SHELL_COMMAND,
            Self::ReadFiles { .. } => READ_FILES,
            Self::Grep { .. } => GREP,
            Self::FindFiles { .. } => FIND_FILES,
            Self::EditFile { .. } => EDIT_FILE,
            Self::CreateFile { .. } => CREATE_FILE,
        }
    }

    /// The call as the model would have made it, for the chat history.
    pub(super) fn to_function_call(&self) -> light_llm::FunctionCall {
        let arguments = match self {
            Self::RunShellCommand { command } => json!({ "command": command }),
            Self::ReadFiles { files } => json!({
                "files": files
                    .iter()
                    .map(|file| match file.lines {
                        Some((start, end)) => {
                            json!({ "path": file.path, "start_line": start, "end_line": end })
                        }
                        None => json!({ "path": file.path }),
                    })
                    .collect::<Vec<_>>(),
            }),
            Self::Grep { queries, path } => json!({ "queries": queries, "path": path }),
            Self::FindFiles { patterns, path } => json!({ "patterns": patterns, "path": path }),
            Self::EditFile {
                path,
                search,
                replace,
            } => json!({ "path": path, "search": search, "replace": replace }),
            Self::CreateFile { path, content } => json!({ "path": path, "content": content }),
        };
        light_llm::FunctionCall {
            name: self.name().to_owned(),
            arguments,
        }
    }

    /// The tool call the client executes.
    #[allow(deprecated)]
    pub(super) fn into_api_tool(self) -> Tool {
        match self {
            // Marked risky and not read-only, which makes the client ask the user for approval
            // unless their allowlist covers the command.
            Self::RunShellCommand { command } => Tool::RunShellCommand(RunShellCommand {
                command,
                is_read_only: false,
                is_risky: true,
                wait_until_complete_value: Some(
                    run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(true),
                ),
                ..Default::default()
            }),
            Self::ReadFiles { files } => Tool::ReadFiles(ReadFiles {
                files: files
                    .into_iter()
                    .map(|file| read_files::File {
                        name: file.path,
                        line_ranges: file
                            .lines
                            .map(|(start, end)| FileContentLineRange { start, end })
                            .into_iter()
                            .collect(),
                    })
                    .collect(),
            }),
            Self::Grep { queries, path } => Tool::Grep(Grep {
                queries,
                path: or_current_dir(path),
            }),
            Self::FindFiles { patterns, path } => Tool::FileGlobV2(FileGlobV2 {
                patterns,
                search_dir: or_current_dir(path),
                ..Default::default()
            }),
            Self::EditFile {
                path,
                search,
                replace,
            } => Tool::ApplyFileDiffs(ApplyFileDiffs {
                summary: format!("Edit {path}"),
                diffs: vec![apply_file_diffs::FileDiff {
                    file_path: path,
                    search,
                    replace,
                }],
                ..Default::default()
            }),
            Self::CreateFile { path, content } => Tool::ApplyFileDiffs(ApplyFileDiffs {
                summary: format!("Create {path}"),
                new_files: vec![apply_file_diffs::NewFile {
                    file_path: path,
                    content,
                    allow_overwrite: false,
                }],
                ..Default::default()
            }),
        }
    }
}

fn or_current_dir(path: String) -> String {
    if path.is_empty() { ".".to_owned() } else { path }
}

/// Accepts a list of strings, a single string, or a JSON-encoded list.
fn string_list(value: Option<&Value>) -> Vec<String> {
    string_or_object_list(value)
        .into_iter()
        .filter_map(|item| item.as_str().map(str::trim).map(str::to_owned))
        .filter(|item| !item.is_empty())
        .collect()
}

fn string_or_object_list(value: Option<&Value>) -> Vec<Value> {
    match value {
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(items)) => items,
            _ => vec![Value::String(text.clone())],
        },
        Some(other @ Value::Object(_)) => vec![other.clone()],
        _ => Vec::new(),
    }
}

fn file_to_read(file: &Value) -> Option<FileToRead> {
    let path = match file {
        Value::String(path) => path.as_str(),
        _ => file.get("path")?.as_str()?,
    }
    .trim();
    if path.is_empty() {
        return None;
    }
    let line = |name: &str| {
        file.get(name)
            .and_then(Value::as_u64)
            .and_then(|line| u32::try_from(line).ok())
            .filter(|line| *line > 0)
    };
    let lines = match (line("start_line"), line("end_line")) {
        (Some(start), Some(end)) if end >= start => Some((start, end)),
        (Some(start), None) => Some((start, start.saturating_add(LINES_READ_FROM_START))),
        (None, Some(end)) => Some((1, end)),
        _ => None,
    };
    Some(FileToRead {
        path: path.to_owned(),
        lines,
    })
}

/// The outcome of a tool the client ran for the local model.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum ToolOutcome {
    Command(RunShellCommandResult),
    ReadFiles(ReadFilesResult),
    Grep(GrepResult),
    FindFiles(FileGlobV2Result),
    FileEdits(ApplyFileDiffsResult),
}

impl ToolOutcome {
    pub(super) fn from_request(result: &RequestResult) -> Option<Self> {
        match result {
            RequestResult::RunShellCommand(result) => Some(Self::Command(result.clone())),
            RequestResult::ReadFiles(result) => Some(Self::ReadFiles(result.clone())),
            RequestResult::Grep(result) => Some(Self::Grep(result.clone())),
            RequestResult::FileGlobV2(result) => Some(Self::FindFiles(result.clone())),
            RequestResult::ApplyFileDiffs(result) => Some(Self::FileEdits(result.clone())),
            _ => None,
        }
    }

    pub(super) fn from_message(result: &MessageResult) -> Option<Self> {
        match result {
            MessageResult::RunShellCommand(result) => Some(Self::Command(result.clone())),
            MessageResult::ReadFiles(result) => Some(Self::ReadFiles(result.clone())),
            MessageResult::Grep(result) => Some(Self::Grep(result.clone())),
            MessageResult::FileGlobV2(result) => Some(Self::FindFiles(result.clone())),
            MessageResult::ApplyFileDiffs(result) => Some(Self::FileEdits(result.clone())),
            _ => None,
        }
    }

    pub(super) fn into_message(self) -> MessageResult {
        match self {
            Self::Command(result) => MessageResult::RunShellCommand(result),
            Self::ReadFiles(result) => MessageResult::ReadFiles(result),
            Self::Grep(result) => MessageResult::Grep(result),
            Self::FindFiles(result) => MessageResult::FileGlobV2(result),
            Self::FileEdits(result) => MessageResult::ApplyFileDiffs(result),
        }
    }

    /// The name of the tool that produced this outcome, used when the call is not known.
    pub(super) fn default_tool_name(&self) -> &'static str {
        match self {
            Self::Command(_) => RUN_SHELL_COMMAND,
            Self::ReadFiles(_) => READ_FILES,
            Self::Grep(_) => GREP,
            Self::FindFiles(_) => FIND_FILES,
            Self::FileEdits(_) => EDIT_FILE,
        }
    }

    /// Describes the outcome for the model.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Command(result) => describe_command_result(result),
            Self::ReadFiles(result) => describe_read_files(result),
            Self::Grep(result) => describe_grep(result),
            Self::FindFiles(result) => describe_find_files(result),
            Self::FileEdits(result) => describe_file_edits(result),
        }
    }
}

fn describe_command_result(result: &RunShellCommandResult) -> String {
    use run_shell_command_result::Result as Outcome;

    match result.result.as_ref() {
        Some(Outcome::CommandFinished(finished)) => format!(
            "Exit code: {}\n{}",
            finished.exit_code,
            truncate_start(&finished.output, MAX_COMMAND_OUTPUT_CHARS)
        ),
        Some(Outcome::LongRunningCommandSnapshot(snapshot)) => format!(
            "The command is still running in the user's terminal, so this is not its final \
             result. Do not conclude that it failed or found nothing. Tell the user it is still \
             running, or try a faster, narrower command. Output so far:\n{}",
            truncate_start(&snapshot.output, MAX_COMMAND_OUTPUT_CHARS)
        ),
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
            truncate_start(&result.output, MAX_COMMAND_OUTPUT_CHARS)
        ),
    }
}

fn describe_read_files(result: &ReadFilesResult) -> String {
    let (files, failed_reads): (Vec<String>, &[read_files_result::FailedRead]) =
        match result.result.as_ref() {
            Some(read_files_result::Result::AnyFilesSuccess(success)) => (
                success
                    .files
                    .iter()
                    .filter_map(|file| match file.content.as_ref()? {
                        any_file_content::Content::TextContent(text) => Some(describe_file(text)),
                        any_file_content::Content::BinaryContent(binary) => Some(format!(
                            "File {} is binary ({} bytes); its content is not shown.",
                            binary.file_path,
                            binary.data.len()
                        )),
                    })
                    .collect(),
                &success.failed_reads,
            ),
            Some(read_files_result::Result::TextFilesSuccess(success)) => (
                success.files.iter().map(describe_file).collect(),
                &success.failed_reads,
            ),
            Some(read_files_result::Result::Error(error)) => {
                return format!("Could not read the files: {}", error.message);
            }
            None => return "The files could not be read.".to_owned(),
        };
    files
        .into_iter()
        .chain(
            failed_reads
                .iter()
                .map(|failed| format!("Could not read {}: {}", failed.path, failed.message)),
        )
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn describe_file(file: &FileContent) -> String {
    let lines = file
        .line_range
        .as_ref()
        .filter(|range| range.start > 0)
        .map(|range| format!(" (lines {}-{})", range.start, range.end))
        .unwrap_or_default();
    let char_count = file.content.chars().count();
    if char_count <= MAX_FILE_CHARS {
        return format!("File {}{lines}:\n{}", file.file_path, file.content);
    }
    let kept: String = file.content.chars().take(MAX_FILE_CHARS).collect();
    let kept_lines = kept.lines().count();
    format!(
        "File {}{lines}:\n{kept}\n[Truncated after {kept_lines} lines of {char_count} characters. \
         Read the rest with start_line and end_line.]",
        file.file_path
    )
}

fn describe_grep(result: &GrepResult) -> String {
    match result.result.as_ref() {
        Some(grep_result::Result::Success(success)) if success.matched_files.is_empty() => {
            "No matches.".to_owned()
        }
        Some(grep_result::Result::Success(success)) => {
            let total = success.matched_files.len();
            let mut lines: Vec<String> = success
                .matched_files
                .iter()
                .take(MAX_LISTED_PATHS)
                .map(|file| {
                    let line_numbers = file
                        .matched_lines
                        .iter()
                        .map(|line| line.line_number.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{}: lines {line_numbers}", file.file_path)
                })
                .collect();
            if total > MAX_LISTED_PATHS {
                lines.push(format!("[{} more files not shown]", total - MAX_LISTED_PATHS));
            }
            lines.join("\n")
        }
        Some(grep_result::Result::Error(error)) => format!("The search failed: {}", error.message),
        None => "The search failed.".to_owned(),
    }
}

fn describe_find_files(result: &FileGlobV2Result) -> String {
    match result.result.as_ref() {
        Some(file_glob_v2_result::Result::Success(success)) => {
            let total = success.matched_files.len();
            let mut lines: Vec<String> = success
                .matched_files
                .iter()
                .take(MAX_LISTED_PATHS)
                .map(|file| file.file_path.clone())
                .collect();
            if total == 0 {
                lines.push("No files found.".to_owned());
            } else if total > MAX_LISTED_PATHS {
                lines.push(format!("[{} more files not shown]", total - MAX_LISTED_PATHS));
            }
            if !success.warnings.is_empty() {
                lines.push(format!("Warnings: {}", success.warnings));
            }
            lines.join("\n")
        }
        Some(file_glob_v2_result::Result::Error(error)) => {
            format!("The search failed: {}", error.message)
        }
        None => "The search failed.".to_owned(),
    }
}

fn describe_file_edits(result: &ApplyFileDiffsResult) -> String {
    match result.result.as_ref() {
        Some(apply_file_diffs_result::Result::Success(success)) => {
            let mut lines = vec!["The changes were applied.".to_owned()];
            for updated in &success.updated_files_v2 {
                let Some(file) = updated.file.as_ref() else {
                    continue;
                };
                if updated.was_edited_by_user {
                    lines.push(format!(
                        "The user modified the change to {} before applying it. Its content \
                         now is:\n{}",
                        file.file_path,
                        truncate_start(&file.content, MAX_FILE_CHARS)
                    ));
                } else {
                    lines.push(format!("Updated {}", file.file_path));
                }
            }
            lines.extend(
                success
                    .deleted_files
                    .iter()
                    .map(|deleted| format!("Deleted {}", deleted.file_path)),
            );
            lines.join("\n")
        }
        Some(apply_file_diffs_result::Result::Error(error)) => format!(
            "The changes could not be applied: {}. Read the file again and retry with text that \
             matches it exactly.",
            error.message
        ),
        None => "The changes could not be applied.".to_owned(),
    }
}

fn truncate_start(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_owned();
    }
    let start = text
        .char_indices()
        .nth(char_count - max_chars)
        .map(|(index, _)| index)
        .unwrap_or_default();
    format!(
        "[Output truncated: showing the last {max_chars} of {char_count} characters]\n{}",
        &text[start..]
    )
}

#[cfg(test)]
#[path = "local_tools_tests.rs"]
mod tests;
