//! Agent-agnostic command-hint synthesis.
//!
//! Different agents (Claude Code, opencode, Cline, Cursor, …) expose tools under
//! different names and argument keys, but the *semantics* fall into a few buckets.
//! This module maps a `(tool_name, input_args)` pair to a pseudo shell command
//! that `compression::command_detector` already understands, so every format
//! handler can reuse the exact same RTK routing / intent-injection that the Codex
//! path enjoys.
//!
//! Design constraints (see project discussion):
//! - Conservative: only well-known tool conventions produce a hint.
//! - Graceful: an unrecognized tool/field returns `None`, so the caller falls
//!   back to content-only detection — i.e. current behavior, never a regression.
//! - Tool-name variance (case, namespacing, snake/camel) is absorbed by
//!   normalization + alias sets, so new agents using common names "just work".

use serde_json::Value;

/// Shell tools: the argument is a raw command line, fed straight to the detector.
const SHELL_TOOLS: &[&str] = &[
    "bash",
    "sh",
    "shell",
    "zsh",
    "exec",
    "exec_command",
    "execcommand",
    "shell_command",
    "shellcommand",
    "execute_command",
    "executecommand",
    "run_command",
    "runcommand",
    "run_terminal_cmd",
    "runterminalcmd",
    "terminal",
    "terminal_command",
];

/// File-read tools: synthesize `cat <path>` → routes to high-fidelity file-read.
const READ_TOOLS: &[&str] = &[
    "read",
    "read_file",
    "readfile",
    "cat",
    "view",
    "view_file",
    "viewfile",
    "open_file",
    "openfile",
    "get_file",
    "getfile",
];

/// Grep/search tools: synthesize `rg <pattern>` → shell-grep + intent injection.
const GREP_TOOLS: &[&str] = &[
    "grep",
    "rg",
    "ripgrep",
    "ag",
    "search",
    "grep_search",
    "grepsearch",
    "search_files",
    "searchfiles",
    "find_text",
    "findtext",
];

/// File-listing tools (glob etc.): synthesize `find <path>` → routes to the
/// `file-list` type, which is high-fidelity (no grouping, char-budget only) so
/// concrete paths are preserved.
const LIST_TOOLS: &[&str] = &[
    "glob",
    "list_files",
    "listfiles",
    "list_dir",
    "listdir",
    "find_files",
    "findfiles",
];

/// Normalize a tool name: lowercase + strip MCP/namespace prefixes
/// (`mcp__server__bash` → `bash`, `namespace.read` → `read`).
fn normalize_tool_name(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    let after_mcp = lower.rsplit("__").next().unwrap_or(&lower);
    let after_dot = after_mcp.rsplit('.').next().unwrap_or(after_mcp);
    after_dot.trim().to_string()
}

/// Read the first present string field from a set of candidate keys.
fn first_str<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(s) = args.get(*k).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Single-quote a value for safe embedding in the synthesized command. We only
/// need detector-parseable text, not real shell execution, so a minimal escape
/// of embedded single quotes is enough.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Parse a tool-call `arguments` value that may be either a JSON object or a
/// JSON-encoded string (OpenAI sends `arguments` as a string).
pub(crate) fn parse_args(arguments: Option<&Value>) -> Value {
    match arguments {
        Some(Value::String(s)) => serde_json::from_str::<Value>(s).unwrap_or(Value::Null),
        Some(v) => v.clone(),
        None => Value::Null,
    }
}

/// Map `(tool_name, args)` to a pseudo command, or `None` to defer to
/// content-based detection.
pub(crate) fn synthesize(tool_name: &str, args: &Value) -> Option<String> {
    if !args.is_object() {
        return None;
    }
    let name = normalize_tool_name(tool_name);

    if SHELL_TOOLS.contains(&name.as_str()) {
        if let Some(cmd) = first_str(args, &["command", "cmd", "script"]) {
            return Some(cmd.to_string());
        }
    }

    if READ_TOOLS.contains(&name.as_str()) {
        if let Some(path) = first_str(args, &["file_path", "filePath", "path", "filename", "file"]) {
            return Some(format!("cat {path}"));
        }
    }

    if GREP_TOOLS.contains(&name.as_str()) {
        if let Some(pattern) = first_str(args, &["pattern", "query", "regex", "search"]) {
            let mut cmd = format!("rg {}", shell_quote(pattern));
            if let Some(p) = first_str(args, &["path", "dir", "directory"]) {
                cmd.push(' ');
                cmd.push_str(p);
            }
            if let Some(g) = first_str(args, &["include", "glob", "type"]) {
                cmd.push_str(&format!(" --glob {}", shell_quote(g)));
            }
            return Some(cmd);
        }
    }

    if LIST_TOOLS.contains(&name.as_str()) {
        let path = first_str(args, &["path", "dir", "directory", "cwd"]).unwrap_or(".");
        return Some(format!("find {path}"));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shell_tool_passes_command_through() {
        let args = json!({"command": "git status", "description": "x"});
        assert_eq!(synthesize("Bash", &args).as_deref(), Some("git status"));
        assert_eq!(synthesize("bash", &args).as_deref(), Some("git status"));
        assert_eq!(
            synthesize("run_terminal_cmd", &json!({"command": "ls -la"})).as_deref(),
            Some("ls -la")
        );
    }

    #[test]
    fn read_tool_synthesizes_cat_with_field_aliases() {
        assert_eq!(
            synthesize("Read", &json!({"file_path": "/a/b.rs"})).as_deref(),
            Some("cat /a/b.rs")
        );
        assert_eq!(
            synthesize("read", &json!({"filePath": "/a/b.rs"})).as_deref(),
            Some("cat /a/b.rs")
        );
    }

    #[test]
    fn grep_tool_synthesizes_rg_with_path_and_include() {
        let args = json!({"pattern": "TODO", "path": "src", "include": "*.rs"});
        assert_eq!(
            synthesize("Grep", &args).as_deref(),
            Some("rg 'TODO' src --glob '*.rs'")
        );
        assert_eq!(
            synthesize("grep", &json!({"pattern": "foo"})).as_deref(),
            Some("rg 'foo'")
        );
    }

    #[test]
    fn glob_tool_routes_to_file_list() {
        assert_eq!(
            synthesize("Glob", &json!({"pattern": "**/*.rs", "path": "src"})).as_deref(),
            Some("find src")
        );
        assert_eq!(
            synthesize("glob", &json!({"pattern": "**/*.ts"})).as_deref(),
            Some("find .")
        );
    }

    #[test]
    fn mcp_prefixed_tool_name_is_normalized() {
        assert_eq!(
            synthesize("mcp__shell__bash", &json!({"command": "pwd"})).as_deref(),
            Some("pwd")
        );
    }

    #[test]
    fn unknown_tool_returns_none() {
        assert_eq!(synthesize("Edit", &json!({"path": "/x"})), None);
        assert_eq!(synthesize("WebFetch", &json!({"url": "http://x"})), None);
        assert_eq!(synthesize("WebSearch", &json!({"query": "rust"})), None);
        assert_eq!(synthesize("Write", &json!({"file_path": "/x"})), None);
    }

    #[test]
    fn non_object_args_returns_none() {
        assert_eq!(synthesize("bash", &Value::Null), None);
        assert_eq!(synthesize("bash", &json!("ls")), None);
    }

    #[test]
    fn parse_args_handles_string_and_object() {
        assert_eq!(parse_args(Some(&json!("{\"cmd\":\"ls\"}"))), json!({"cmd": "ls"}));
        assert_eq!(parse_args(Some(&json!({"cmd": "ls"}))), json!({"cmd": "ls"}));
        assert_eq!(parse_args(None), Value::Null);
    }
}
