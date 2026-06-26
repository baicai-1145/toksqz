use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};

use toksqz::compression::command_detector::{detect_command_type, normalize_command_for_hint};

fn main() {
    let path = env::args().nth(1).expect("usage: session_cmd_audit <commands.jsonl>");
    let file = File::open(path).expect("open commands file");
    let reader = BufReader::new(file);

    let mut total = 0usize;
    let mut unknown = 0usize;
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_norm: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_buckets: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_short_buckets: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_long_buckets: BTreeMap<String, usize> = BTreeMap::new();
    let mut python_forms: BTreeMap<String, usize> = BTreeMap::new();
    let mut unknown_short = 0usize;
    let mut unknown_long = 0usize;

    const PASSTHROUGH_MAX_LINES: usize = 30;
    const PASSTHROUGH_MAX_CHARS: usize = 2000;

    for line in reader.lines() {
        let raw = line.expect("read line");
        if raw.trim().is_empty() {
            continue;
        }
        let cmd: String = serde_json::from_str(&raw).unwrap_or(raw);
        if cmd.trim().is_empty() {
            continue;
        }
        total += 1;
        let detection = detect_command_type("", Some(&cmd));
        *by_type.entry(detection.command_type.clone()).or_default() += 1;
        if detection.command_type == "unknown" {
            unknown += 1;
            let is_short = cmd.len() <= PASSTHROUGH_MAX_CHARS
                && cmd.bytes().filter(|&b| b == b'\n').count() < PASSTHROUGH_MAX_LINES;
            if is_short {
                unknown_short += 1;
            } else {
                unknown_long += 1;
            }
            let norm = normalize_command_for_hint(&cmd);
            let head = norm.split_whitespace().next().unwrap_or("<empty>").to_string();
            let bucket = categorize_unknown(&norm, &head);
            *unknown_norm.entry(head).or_default() += 1;
            *unknown_buckets.entry(bucket.to_string()).or_default() += 1;
            if is_short {
                *unknown_short_buckets.entry(bucket.to_string()).or_default() += 1;
            } else {
                *unknown_long_buckets.entry(bucket.to_string()).or_default() += 1;
            }
            if bucket == "python-gap" {
                *python_forms.entry(python_form(&norm).to_string()).or_default() += 1;
            }
        }
    }

    println!("total\t{}", total);
    println!("unknown\t{}", unknown);
    println!(
        "unknown_pct\t{:.2}",
        unknown as f64 / total.max(1) as f64 * 100.0
    );
    println!("unknown_short_passthrough\t{}", unknown_short);
    println!(
        "unknown_short_pct\t{:.2}",
        unknown_short as f64 / total.max(1) as f64 * 100.0
    );
    println!("unknown_long_generic\t{}", unknown_long);
    println!(
        "unknown_long_pct\t{:.2}",
        unknown_long as f64 / total.max(1) as f64 * 100.0
    );
    println!("---by_type---");
    let mut types: Vec<_> = by_type.iter().collect();
    types.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in types {
        println!("{}\t{}", k, v);
    }
    println!("---unknown_buckets---");
    print_bucket_map(&unknown_buckets, total);
    println!("---unknown_short_buckets---");
    print_bucket_map(&unknown_short_buckets, total);
    println!("---unknown_long_buckets---");
    print_bucket_map(&unknown_long_buckets, total);
    println!("---python_gap_forms---");
    print_bucket_map(&python_forms, total);
    println!("---unknown_heads---");
    let mut heads: Vec<_> = unknown_norm.iter().collect();
    heads.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in heads.iter().take(50) {
        println!("{}\t{}", k, v);
    }
}

fn categorize_unknown(norm: &str, head: &str) -> &'static str {
    let lower = norm.to_lowercase();
    if lower.starts_with("python") || head == "python" || head == "python3" || head.ends_with("/python") {
        return "python-gap";
    }
    match head {
        "cd" | "rm" | "mkdir" | "export" | "set" | "kill" | "pkill" | "echo" | "printf"
        | "sleep" | "pwd" | "source" | "test" | "which" | "date" | "wc" | "if" | "for" => {
            return "shell-control";
        }
        "git" => return "git-partial",
        "node" | "npm" | "npx" | "pnpm" | "yarn" => return "node-npm",
        "cargo" | "swift" | "go" => return "build-partial",
        "brew" | "uv" | "codex" => return "tooling",
        "sudo" => return "privileged",
        _ if head.starts_with("./") || head.starts_with("/") || head.contains('/') => {
            return "custom-binary";
        }
        _ if head.starts_with("runtime/") || head.ends_with(".sh") || head.ends_with(".py") => {
            return "custom-binary";
        }
        _ => "other",
    }
}

/// Classify the *form* of a python invocation (already normalized, head is python*).
fn python_form(norm: &str) -> &'static str {
    let rest = norm.strip_prefix("python3").or_else(|| norm.strip_prefix("python")).unwrap_or(norm);
    let rest = rest.trim_start();
    if rest.starts_with("- ") || rest == "-" || rest.starts_with("- <") || rest.starts_with("-<") {
        return "stdin/heredoc (python -)";
    }
    if rest.starts_with("-c") {
        return "-c inline";
    }
    if rest.starts_with("-m ") {
        let module = rest[3..].split_whitespace().next().unwrap_or("");
        return match module {
            "py_compile" => "-m py_compile",
            "json.tool" => "-m json.tool",
            "http.server" => "-m http.server",
            "venv" => "-m venv",
            "pip" | "pytest" | "mypy" => "-m (should-be-other!)",
            _ => "-m other module",
        };
    }
    if rest.starts_with("--version") || rest.starts_with("-V") {
        return "--version";
    }
    if rest.is_empty() {
        return "bare python";
    }
    if rest.split_whitespace().next().map(|t| t.ends_with(".py")).unwrap_or(false) {
        return ".py file (should-match!)";
    }
    "other"
}

fn print_bucket_map(map: &BTreeMap<String, usize>, total: usize) {
    let mut rows: Vec<_> = map.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in rows {
        println!(
            "{}\t{}\t{:.2}",
            k,
            v,
            *v as f64 / total.max(1) as f64 * 100.0
        );
    }
}
