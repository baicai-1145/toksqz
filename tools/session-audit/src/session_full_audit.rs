use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use toksqz::compression::command_detector::extract_intent_keywords;
use toksqz::compression::{rtk_compress_with_command, RtkCompressResult};

struct ExecRecord {
    cmd: String,
    output: String,
    orig_tokens: Option<u64>,
}

fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() + 3) / 4
}

fn extract_output_body(raw: &str) -> String {
    if let Some(idx) = raw.find("\nOutput:\n") {
        return raw[idx + 9..].to_string();
    }
    if let Some(idx) = raw.find("Output:\n") {
        return raw[idx + 8..].to_string();
    }
    raw.to_string()
}

fn collect_sessions(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(collect_sessions(&path));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push(path);
            }
        }
    }
    files
}

fn load_exec_records(root: &Path) -> (Vec<ExecRecord>, usize, usize) {
    static TOK_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let tok_re = TOK_RE.get_or_init(|| Regex::new(r"Original token count:\s*(\d+)").unwrap());

    let mut cmd_by_id: BTreeMap<String, String> = BTreeMap::new();
    let mut out_by_id: BTreeMap<String, (String, Option<u64>)> = BTreeMap::new();
    let mut exec_calls = 0usize;

    for path in collect_sessions(root) {
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        for line in content.lines() {
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if obj.get("type").and_then(|v| v.as_str()) != Some("response_item") {
                continue;
            }
            let item = obj
                .get("payload")
                .and_then(|p| p.get("item").or(Some(p)))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let Some(item_type) = item.get("type").and_then(|v| v.as_str()) else {
                continue;
            };
            match item_type {
                "function_call" => {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    if name != "exec_command" && name != "shell_command" {
                        continue;
                    }
                    exec_calls += 1;
                    let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let args_val = item.get("arguments").cloned().unwrap_or_default();
                    let args: serde_json::Value = if let Some(s) = args_val.as_str() {
                        serde_json::from_str(s).unwrap_or(args_val)
                    } else {
                        args_val
                    };
                    let cmd = args
                        .get("cmd")
                        .or_else(|| args.get("command"))
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Array(arr) => arr
                                .iter()
                                .map(|x| x.as_str().unwrap_or(""))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            _ => v.to_string(),
                        });
                    if let Some(cmd) = cmd {
                        if !cmd.trim().is_empty() {
                            cmd_by_id.insert(call_id.to_string(), cmd);
                        }
                    }
                }
                "function_call_output" => {
                    let Some(call_id) = item.get("call_id").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let output = item
                        .get("output")
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            _ => v.to_string(),
                        })
                        .unwrap_or_default();
                    let orig_tokens = tok_re
                        .captures(&output)
                        .and_then(|c| c.get(1))
                        .and_then(|m| m.as_str().parse().ok());
                    out_by_id.insert(call_id.to_string(), (output, orig_tokens));
                }
                _ => {}
            }
        }
    }

    let mut records = Vec::new();
    for (call_id, cmd) in cmd_by_id {
        if let Some((output, orig_tokens)) = out_by_id.remove(&call_id) {
            records.push(ExecRecord {
                cmd,
                output: extract_output_body(&output),
                orig_tokens,
            });
        }
    }

    (records, exec_calls, out_by_id.len())
}

/// Strip ANSI/CSI escape sequences (local copy; lib's strip_ansi is pub(crate)).
fn strip_ansi_local(s: &str) -> String {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap());
    re.replace_all(s, "").into_owned()
}

/// Lossless "safe" cleanup baseline: strip ANSI, drop pure-blank lines, and
/// collapse runs of identical consecutive lines into one. These are the only
/// transformations that provably lose no information for an agent. The size of
/// this output is the floor any compressor can hit without risking data loss.
fn safe_clean(s: &str) -> String {
    let no_ansi = strip_ansi_local(s);
    let mut out: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;
    for line in no_ansi.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if prev.as_deref() == Some(line) {
            continue;
        }
        out.push(line.to_string());
        prev = Some(line.to_string());
    }
    out.join("\n")
}

/// Extract the file path being read, ignoring line ranges, so repeated reads of the
/// same file (even at different line windows) collapse to one key.
fn normalize_read_target(cmd: &str) -> String {
    let first = cmd.lines().next().unwrap_or("").trim();
    // Take the segment that actually reads (after the last connector).
    let seg = first
        .rsplit("&&")
        .next()
        .unwrap_or(first)
        .rsplit('|')
        .next()
        .unwrap_or(first)
        .trim();
    let mut path = String::new();
    for tok in seg.split_whitespace() {
        // The path is usually the last non-flag, non-numeric-range token.
        if tok.starts_with('-') || tok.chars().all(|c| c.is_ascii_digit() || c == ',' || c == 'p' || c == '\'' || c == '"') {
            continue;
        }
        if tok == "cat" || tok == "bat" || tok == "sed" || tok == "nl" || tok == "head" || tok == "tail" || tok == "-n" {
            continue;
        }
        if tok.contains('/') || tok.contains('.') {
            path = tok.to_string();
        }
    }
    if path.is_empty() {
        seg.chars().take(60).collect()
    } else {
        path
    }
}

fn is_passthrough(output: &str, result: &RtkCompressResult) -> bool {
    const PASSTHROUGH_MAX_LINES: usize = 30;
    const PASSTHROUGH_MAX_CHARS: usize = 2000;
    result.command_type == "unknown"
        && result.filter_id.is_none()
        && !result.grouping_applied
        && output.len() <= PASSTHROUGH_MAX_CHARS
        && output.bytes().filter(|&b| b == b'\n').count() < PASSTHROUGH_MAX_LINES
}

fn main() {
    let root = env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        let home = env::var("HOME").expect("HOME");
        PathBuf::from(home).join(".codex/sessions")
    });

    eprintln!("scanning sessions under {}", root.display());
    let (records, exec_calls, orphan_outputs) = load_exec_records(&root);
    eprintln!(
        "exec_command calls: {}, paired with output: {}, orphan outputs: {}",
        exec_calls,
        records.len(),
        orphan_outputs
    );

    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_filter: BTreeMap<String, usize> = BTreeMap::new();
    let mut passthrough = 0usize;
    let mut compressed_smaller = 0usize;
    let mut unchanged = 0usize;
    let mut truncated = 0usize;
    let mut intent_used = 0usize;
    let mut intent_saved_on_trunc = 0usize;

    let mut orig_chars: u64 = 0;
    let mut out_chars: u64 = 0;
    let mut orig_tokens_est: u64 = 0;
    let mut out_tokens_est: u64 = 0;
    let mut codex_orig_tokens: u64 = 0;
    let mut codex_pairs = 0usize;

    // Under-compression tracking: large outputs that barely shrank.
    // "large" = output above this token threshold after compression.
    const BIG_TOKENS: usize = 1000;
    // "barely shrank" = kept >= this fraction of original chars.
    const LOW_REDUCTION: f64 = 0.90;
    let mut under_count = 0usize;
    let mut under_retained_tokens: u64 = 0;
    // Per command_type: (count, retained_out_tokens) for under-compressed big outputs.
    let mut under_by_type: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    // Top offenders by retained tokens.
    let mut offenders: Vec<(usize, String, String, usize, usize)> = Vec::new();
    // Split: capped at global budget vs genuinely below-budget uncompressed.
    let mut under_at_budget = 0usize;
    let mut under_below_budget = 0usize;
    let mut under_below_tokens: u64 = 0;
    let mut under_below_by_type: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    let mut redundant_blank_lines: u64 = 0;
    let mut redundant_dup_lines: u64 = 0;
    // Per command_type: (count, orig_tok, out_tok) to compute per-type reduction.
    let mut ratio_by_type: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();

    // Safe-baseline framework, per command_type.
    // Tuple: (count,
    //         orig_chars,
    //         actual_out_chars,
    //         safe_baseline_chars,            // chars after lossless cleanup only
    //         unutilized_safe_chars,          // sum over records of max(0, actual-safe): safe room left on the table
    //         lossy_chars,                    // sum over records of max(0, safe-actual): cut beyond lossless floor
    //         records_with_unutilized,        // count of records still holding safe-removable redundancy
    //         records_lossy)                  // count of records compressed past the lossless floor
    #[derive(Default)]
    struct SafeStat {
        count: usize,
        orig_chars: u64,
        actual_chars: u64,
        safe_chars: u64,
        unutilized: u64,
        lossy: u64,
        recs_unutilized: usize,
        recs_lossy: usize,
        // Split of `lossy`: chars cut below the lossless floor on records whose
        // ORIGINAL output already exceeded the global char budget (truncation was
        // expected/safe) vs records under budget (rules dropped real lines =
        // genuine over-compression suspect).
        lossy_budget: u64,
        lossy_rules: u64,
        recs_lossy_rules: usize,
    }
    // Mirror of toksqz's GLOBAL_MAX_CHARS default; outputs above this are expected
    // to be char-truncated regardless of type, so lossy beyond it is not "suspect".
    const GLOBAL_BUDGET: u64 = 12_000;
    let mut safe_by_type: BTreeMap<String, SafeStat> = BTreeMap::new();

    // file-read deep dive.
    let mut fr_count = 0usize;
    let mut fr_tokens: u64 = 0;
    let mut fr_line_buckets: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fr_over_budget = 0usize; // output > 12k chars (would be char-truncated)
    // Hypothetical maxLines caps: how many records exceed, and tokens saved if we
    // kept only head+tail = cap lines (rough: assume avg chars/line constant).
    let caps = [200usize, 300, 500];
    let mut fr_cap_exceed = [0usize; 3];
    let mut fr_cap_saved_tokens = [0u64; 3];
    // Repeated reads: same normalized read command seen multiple times.
    let mut fr_cmd_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut fr_cmd_tokens: BTreeMap<String, u64> = BTreeMap::new();

    // Generic grouping investigation: unknown-type records whose output was folded
    // by group_file_list. Track which commands trigger it and sample what got folded.
    let mut gg_count = 0usize;
    let mut gg_tokens_cut: u64 = 0;
    let mut gg_by_cmd_head: BTreeMap<String, usize> = BTreeMap::new();
    let mut gg_samples: Vec<(String, usize, usize, String)> = Vec::new(); // (cmd1stline, orig_lines, out_lines, out_text)
    // Attribution of unknown rule-lossy records by sub-cause.
    let mut unk_cause_grouped = 0usize;
    let mut unk_cause_trunc = 0usize;
    let mut unk_cause_other = 0usize;
    let mut unk_other_samples: Vec<(String, String, usize, usize, String)> = Vec::new();

    for rec in &records {
        let intent = extract_intent_keywords(&rec.cmd);
        let result = rtk_compress_with_command(&rec.output, Some(&rec.cmd));

        *by_type.entry(result.command_type.clone()).or_default() += 1;
        let fid = result
            .filter_id
            .clone()
            .unwrap_or_else(|| "passthrough/none".to_string());
        *by_filter.entry(fid).or_default() += 1;

        if is_passthrough(&rec.output, &result) {
            passthrough += 1;
        }
        if result.text.contains("[rtk:truncated") {
            truncated += 1;
            if !intent.is_empty() {
                intent_saved_on_trunc += 1;
            }
        }
        if !intent.is_empty() {
            intent_used += 1;
        }

        let o_chars = rec.output.len() as u64;
        let c_chars = result.text.len() as u64;
        orig_chars += o_chars;
        out_chars += c_chars;
        let o_tok = estimate_tokens(&rec.output) as u64;
        let c_tok = estimate_tokens(&result.text) as u64;
        orig_tokens_est += o_tok;
        out_tokens_est += c_tok;
        let rt = ratio_by_type.entry(result.command_type.clone()).or_default();
        rt.0 += 1;
        rt.1 += o_tok;
        rt.2 += c_tok;

        // Safe-baseline comparison. Use byte lengths throughout for consistency
        // with o_chars/c_chars (.len()); mixing chars vs bytes would mis-flag
        // multibyte output as lossy.
        let safe = safe_clean(&rec.output);
        let safe_n = safe.len() as u64;
        let actual_n = c_chars as u64;
        let orig_n = o_chars as u64;
        let st = safe_by_type.entry(result.command_type.clone()).or_default();
        st.count += 1;
        st.orig_chars += orig_n;
        st.actual_chars += actual_n;
        // Cap safe baseline at orig (never larger).
        let safe_eff = safe_n.min(orig_n);
        st.safe_chars += safe_eff;
        if actual_n > safe_eff {
            st.unutilized += actual_n - safe_eff;
            st.recs_unutilized += 1;
        } else if safe_eff > actual_n {
            let cut = safe_eff - actual_n;
            st.lossy += cut;
            st.recs_lossy += 1;
            if orig_n > GLOBAL_BUDGET {
                // Expected: output was large enough to hit the global char budget.
                st.lossy_budget += cut;
            } else {
                // Suspect: output fit within budget yet rules still dropped real
                // (non-blank, non-duplicate) lines.
                st.lossy_rules += cut;
                st.recs_lossy_rules += 1;
                if result.command_type == "unknown" {
                    if result.grouping_applied {
                        unk_cause_grouped += 1;
                    } else if result.text.contains("[rtk:truncated") {
                        unk_cause_trunc += 1;
                    } else {
                        unk_cause_other += 1;
                        if unk_other_samples.len() < 10 {
                            let fid = result.filter_id.clone().unwrap_or_else(|| "none".into());
                            unk_other_samples.push((
                                rec.cmd.lines().next().unwrap_or("").chars().take(70).collect(),
                                fid,
                                rec.output.split('\n').count(),
                                result.text.split('\n').count(),
                                result.text.chars().take(300).collect(),
                            ));
                        }
                    }
                }
            }
        }

        if result.command_type == "file-read" {
            fr_count += 1;
            fr_tokens += c_tok;
            let nlines = result.text.split('\n').count();
            let bucket = match nlines {
                0..=50 => "00-50",
                51..=150 => "051-150",
                151..=300 => "151-300",
                301..=500 => "301-500",
                501..=1000 => "0501-1000",
                _ => "1000+",
            };
            *fr_line_buckets.entry(bucket).or_default() += 1;
            if c_chars > 12_000 {
                fr_over_budget += 1;
            }
            let avg_chars_per_line = c_chars as f64 / nlines.max(1) as f64;
            for (i, &cap) in caps.iter().enumerate() {
                if nlines > cap {
                    fr_cap_exceed[i] += 1;
                    let dropped_lines = (nlines - cap) as f64;
                    let saved_chars = dropped_lines * avg_chars_per_line;
                    fr_cap_saved_tokens[i] += (saved_chars / 4.0) as u64;
                }
            }
            // Normalize the read command (strip line-range args) to detect repeats.
            let key = normalize_read_target(&rec.cmd);
            *fr_cmd_seen.entry(key.clone()).or_default() += 1;
            *fr_cmd_tokens.entry(key).or_default() += c_tok;
        }

        if result.command_type == "unknown" && result.grouping_applied {
            gg_count += 1;
            gg_tokens_cut += o_tok.saturating_sub(c_tok);
            let head = rec
                .cmd
                .lines()
                .next()
                .unwrap_or("")
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            *gg_by_cmd_head.entry(head).or_default() += 1;
            if gg_samples.len() < 12 {
                let first_line = rec.cmd.lines().next().unwrap_or("").chars().take(80).collect();
                gg_samples.push((
                    first_line,
                    rec.output.split('\n').count(),
                    result.text.split('\n').count(),
                    result.text.chars().take(400).collect(),
                ));
            }
        }

        if let Some(t) = rec.orig_tokens {
            codex_orig_tokens += t;
            codex_pairs += 1;
        }

        if c_chars < o_chars {
            compressed_smaller += 1;
        } else {
            unchanged += 1;
        }

        // Under-compression: output still big AND barely reduced.
        let out_tok = estimate_tokens(&result.text);
        let retained_ratio = c_chars as f64 / o_chars.max(1) as f64;
        if out_tok >= BIG_TOKENS && retained_ratio >= LOW_REDUCTION {
            under_count += 1;
            under_retained_tokens += out_tok as u64;
            let e = under_by_type.entry(result.command_type.clone()).or_default();
            e.0 += 1;
            e.1 += out_tok as u64;
            let cmd_head: String = rec.cmd.lines().next().unwrap_or("").chars().take(70).collect();
            offenders.push((
                out_tok,
                result.command_type.clone(),
                cmd_head,
                estimate_tokens(&rec.output),
                out_tok,
            ));

            // Is this capped at the global char budget, or below it (genuinely uncompressed)?
            if c_chars >= 11_500 {
                under_at_budget += 1;
            } else {
                under_below_budget += 1;
                under_below_tokens += out_tok as u64;
                // Removable redundancy in the *compressed* output: blank lines and
                // consecutive duplicate non-blank lines that survived.
                let mut blanks = 0usize;
                let mut dups = 0usize;
                let mut prev: Option<&str> = None;
                for line in result.text.split('\n') {
                    if line.trim().is_empty() {
                        blanks += 1;
                    } else if prev == Some(line) {
                        dups += 1;
                    }
                    prev = Some(line);
                }
                redundant_blank_lines += blanks as u64;
                redundant_dup_lines += dups as u64;
                let e2 = under_below_by_type.entry(result.command_type.clone()).or_default();
                e2.0 += 1;
                e2.1 += out_tok as u64;
            }
        }
    }

    let n = records.len().max(1) as f64;
    println!("paired_records\t{}", records.len());
    println!("orig_chars\t{}", orig_chars);
    println!("out_chars\t{}", out_chars);
    println!(
        "char_reduction_pct\t{:.2}",
        (1.0 - out_chars as f64 / orig_chars.max(1) as f64) * 100.0
    );
    println!("orig_tokens_est\t{}", orig_tokens_est);
    println!("out_tokens_est\t{}", out_tokens_est);
    println!(
        "token_reduction_pct\t{:.2}",
        (1.0 - out_tokens_est as f64 / orig_tokens_est.max(1) as f64) * 100.0
    );
    if codex_pairs > 0 {
        println!("codex_orig_tokens\t{}", codex_orig_tokens);
        println!(
            "codex_vs_toksqz_out_tokens_pct\t{:.2}",
            (1.0 - out_tokens_est as f64 / codex_orig_tokens.max(1) as f64) * 100.0
        );
    }
    println!("passthrough\t{}", passthrough);
    println!("passthrough_pct\t{:.2}", passthrough as f64 / n * 100.0);
    println!("compressed_smaller\t{}", compressed_smaller);
    println!("unchanged_or_larger\t{}", unchanged);
    println!("truncated\t{}", truncated);
    println!("truncated_pct\t{:.2}", truncated as f64 / n * 100.0);
    println!("intent_keywords_used\t{}", intent_used);
    println!("intent_on_truncated\t{}", intent_saved_on_trunc);

    println!("---by_type---");
    let mut types: Vec<_> = by_type.iter().collect();
    types.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in types {
        println!("{}\t{}", k, v);
    }

    println!("---by_filter---");
    let mut filters: Vec<_> = by_filter.iter().collect();
    filters.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in filters.iter().take(30) {
        println!("{}\t{}", k, v);
    }

    println!("---under_compression (out>={} tok AND kept>={:.0}% chars)---", BIG_TOKENS, LOW_REDUCTION * 100.0);
    println!("under_count\t{}", under_count);
    println!("under_pct_of_records\t{:.2}", under_count as f64 / n * 100.0);
    println!("under_retained_tokens\t{}", under_retained_tokens);
    println!(
        "under_retained_pct_of_out\t{:.2}",
        under_retained_tokens as f64 / out_tokens_est.max(1) as f64 * 100.0
    );
    println!("--- under_by_type (count, retained_tokens) ---");
    let mut ut: Vec<_> = under_by_type.iter().collect();
    ut.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (k, (c, t)) in ut {
        println!("{}\t{}\t{}", k, c, t);
    }
    println!("--- reduction_by_type (count, orig_tok, out_tok, reduction%) sorted by orig_tok ---");
    let mut rbt: Vec<_> = ratio_by_type.iter().collect();
    rbt.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (ty, (cnt, ot, ct)) in rbt {
        let red = (1.0 - *ct as f64 / (*ot).max(1) as f64) * 100.0;
        println!("{}\t{}\t{}\t{}\t{:.1}", ty, cnt, ot, ct, red);
    }

    // Safe-baseline report: where is there safe room left, and who over-compresses.
    println!("--- safe_baseline_by_type ---");
    println!("# type | n | orig_tok | actual_tok | safe_floor_tok | unutilized_tok (recs) | lossy_tok (recs)");
    let mut sbt: Vec<_> = safe_by_type.iter().collect();
    sbt.sort_by(|a, b| b.1.orig_chars.cmp(&a.1.orig_chars));
    for (ty, s) in &sbt {
        println!(
            "{}\tn={}\torig={}\tactual={}\tsafe_floor={}\tunutilized={} (recs={})\tlossy={} (recs={})",
            ty,
            s.count,
            s.orig_chars / 4,
            s.actual_chars / 4,
            s.safe_chars / 4,
            s.unutilized / 4,
            s.recs_unutilized,
            s.lossy / 4,
            s.recs_lossy,
        );
    }

    println!("--- safe_space_remaining (sorted by unutilized_tok desc) ---");
    println!("# types still holding lossless-removable redundancy after toksqz");
    let mut by_unutil: Vec<_> = safe_by_type.iter().collect();
    by_unutil.sort_by(|a, b| b.1.unutilized.cmp(&a.1.unutilized));
    for (ty, s) in by_unutil.iter().take(15) {
        if s.unutilized == 0 {
            continue;
        }
        let pct_of_actual = s.unutilized as f64 / s.actual_chars.max(1) as f64 * 100.0;
        println!(
            "{}\tunutilized_tok={}\t({:.1}% of its actual output)\trecs={}/{}",
            ty,
            s.unutilized / 4,
            pct_of_actual,
            s.recs_unutilized,
            s.count,
        );
    }

    println!("--- lossy_split_by_type (sorted by total lossy_tok desc) ---");
    println!("# lossy = cut below lossless floor. budget = expected (orig>12k, truncated).");
    println!("# rules = SUSPECT (orig<=12k yet real lines dropped by filter rules).");
    let mut by_lossy: Vec<_> = safe_by_type.iter().collect();
    by_lossy.sort_by(|a, b| b.1.lossy.cmp(&a.1.lossy));
    for (ty, s) in by_lossy.iter().take(20) {
        if s.lossy == 0 {
            continue;
        }
        println!(
            "{}\tlossy_tok={}\tbudget={}\trules={} (recs={}/{})",
            ty,
            s.lossy / 4,
            s.lossy_budget / 4,
            s.lossy_rules / 4,
            s.recs_lossy_rules,
            s.count,
        );
    }

    println!("--- over_compression_SUSPECTS (rule-driven, sorted by lossy_rules_tok desc) ---");
    println!("# under budget yet dropped real lines: these are the only true over-compression risks");
    let mut by_rules: Vec<_> = safe_by_type.iter().collect();
    by_rules.sort_by(|a, b| b.1.lossy_rules.cmp(&a.1.lossy_rules));
    for (ty, s) in by_rules.iter().take(20) {
        if s.lossy_rules == 0 {
            continue;
        }
        // Average chars cut per suspect record => is it trimming a little or gutting?
        let avg_cut_tok = s.lossy_rules / 4 / s.recs_lossy_rules.max(1) as u64;
        println!(
            "{}\tlossy_rules_tok={}\trecs={}/{}\tavg_cut_tok/rec={}",
            ty,
            s.lossy_rules / 4,
            s.recs_lossy_rules,
            s.count,
            avg_cut_tok,
        );
    }

    println!("--- unknown_lossy_cause_breakdown ---");
    println!("grouped\t{}", unk_cause_grouped);
    println!("trunc_marker\t{}", unk_cause_trunc);
    println!("other\t{}", unk_cause_other);
    println!("--- unk_other_samples (cmd | filter | lines in->out | result head) ---");
    for (cmd, fid, il, ol, out) in &unk_other_samples {
        println!("CMD: {}  [filter={}]", cmd, fid);
        println!("  lines {} -> {}", il, ol);
        println!("  OUT: {}", out.replace('\n', " | "));
    }

    println!("--- generic_grouping_investigation ---");
    println!("gg_count(unknown+grouped)\t{}", gg_count);
    println!("gg_tokens_cut~\t{}", gg_tokens_cut);
    println!("--- gg_by_cmd_head (sorted) ---");
    let mut gghead: Vec<_> = gg_by_cmd_head.iter().collect();
    gghead.sort_by(|a, b| b.1.cmp(a.1));
    for (h, c) in gghead.iter().take(25) {
        println!("{}\t{}", c, h);
    }
    println!("--- gg_samples (cmd | orig_lines->out_lines | folded output head) ---");
    for (cmd, ol, nl, out) in &gg_samples {
        println!("CMD: {}", cmd);
        println!("  lines {} -> {}", ol, nl);
        println!("  OUT: {}", out.replace('\n', " | "));
    }

    println!("--- file_read_deep_dive ---");
    println!("fr_count\t{}", fr_count);
    println!("fr_tokens\t{}", fr_tokens);
    println!("fr_over_12k_chars\t{}", fr_over_budget);
    println!("--- fr_line_buckets ---");
    for (k, v) in &fr_line_buckets {
        println!("{}\t{}\t{:.1}", k, v, *v as f64 / fr_count.max(1) as f64 * 100.0);
    }
    println!("--- fr_hypothetical_maxlines_caps ---");
    for (i, &cap) in caps.iter().enumerate() {
        println!(
            "cap={}\texceed={}\t({:.1}% of file-read)\tsaved_tokens~{}\t({:.1}% of fr_tokens, {:.2}% of all_out_tokens)",
            cap,
            fr_cap_exceed[i],
            fr_cap_exceed[i] as f64 / fr_count.max(1) as f64 * 100.0,
            fr_cap_saved_tokens[i],
            fr_cap_saved_tokens[i] as f64 / fr_tokens.max(1) as f64 * 100.0,
            fr_cap_saved_tokens[i] as f64 / out_tokens_est.max(1) as f64 * 100.0,
        );
    }
    println!("--- fr_repeated_reads ---");
    let repeated: usize = fr_cmd_seen.values().filter(|&&c| c > 1).count();
    let repeat_total_reads: usize = fr_cmd_seen.values().filter(|&&c| c > 1).map(|&c| c).sum();
    let unique: usize = fr_cmd_seen.len();
    println!("unique_targets\t{}", unique);
    println!("targets_read_more_than_once\t{}", repeated);
    println!("total_reads_of_repeated_targets\t{}", repeat_total_reads);
    let mut rep: Vec<_> = fr_cmd_seen.iter().filter(|(_, &c)| c > 1).collect();
    rep.sort_by(|a, b| {
        let ta = fr_cmd_tokens.get(a.0).copied().unwrap_or(0);
        let tb = fr_cmd_tokens.get(b.0).copied().unwrap_or(0);
        tb.cmp(&ta)
    });
    println!("--- top_repeated_targets (reads, total_tokens, target) ---");
    for (k, c) in rep.iter().take(20) {
        println!("{}\t{}\t{}", c, fr_cmd_tokens.get(*k).copied().unwrap_or(0), k);
    }

    println!("--- under_split ---");
    println!("at_budget (capped ~12k, working)\t{}", under_at_budget);
    println!("below_budget (genuinely uncompressed)\t{}", under_below_budget);
    println!("below_budget_retained_tokens\t{}", under_below_tokens);
    println!(
        "below_budget_pct_of_out\t{:.2}",
        under_below_tokens as f64 / out_tokens_est.max(1) as f64 * 100.0
    );
    println!("redundant_blank_lines_in_below\t{}", redundant_blank_lines);
    println!("redundant_consecutive_dup_lines_in_below\t{}", redundant_dup_lines);
    println!("--- below_budget_by_type (count, retained_tokens) ---");
    let mut ubt: Vec<_> = under_below_by_type.iter().collect();
    ubt.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    for (k, (c, t)) in ubt.iter().take(20) {
        println!("{}\t{}\t{}", k, c, t);
    }
    println!("--- top_offenders (out_tok, type, orig_tok, cmd) ---");
    offenders.sort_by(|a, b| b.0.cmp(&a.0));
    for (out_tok, ty, cmd, orig_tok, _) in offenders.iter().take(25) {
        println!("{}\t{}\t{}\t{}", out_tok, ty, orig_tok, cmd);
    }
}
