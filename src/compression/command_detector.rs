use once_cell::sync::Lazy;
use regex::Regex;

pub struct CommandDetectionResult {
    pub command_type: String,
    pub command: Option<String>,
    pub confidence: f64,
    pub category: String,
}

static COMMAND_PREFIXES: &[&str] = &[
    "git", "make", "terraform", "tofu", "opentofu", "systemctl",
    "npm", "pnpm", "yarn", "vitest", "jest", "pytest", "python",
    "go", "cargo", "tsc", "eslint", "webpack", "vite", "biome",
    "prettier", "turbo", "nx", "playwright", "ruff", "mypy",
    "pip", "uv", "poetry", "golangci-lint", "bundle", "rubocop",
    "kubectl", "composer", "gh", "docker", "aws", "gcloud",
    "ssh", "rsync", "curl", "wget", "ls", "find", "fd", "grep", "rg",
    "ag", "cat", "bat", "sed", "nl", "head", "tail", "jq", "yq",
    "ps", "df", "du",
    "otool", "nm", "strings", "dwarfdump", "lipo", "vtool", "atos",
    "swift", "clang", "gcc", "g++", "clang++",
    "sysctl", "sw_vers", "kmutil", "codesign", "plutil", "spctl",
    "memory_pressure", "vm_stat", "ioreg", "system_profiler", "dyld_info",
    "lldb", "kextstat",
];

static COMMAND_PREFIX_PATTERN: Lazy<Regex> = Lazy::new(|| {
    let pattern = format!("^(?:{})\\b", COMMAND_PREFIXES.join("|"));
    Regex::new(&pattern).unwrap()
});

static PYTHON_PATH_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:\S*/)?python(?:\d+(?:\.\d+)?)?\b").unwrap()
});

static ENV_PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"^(?:[A-Za-z_][A-Za-z0-9_]*=(?:"[^"]*"|'[^']*'|\S+)\s+)+"#).unwrap()
});

static SHELL_WRAPPER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:env\s+|command\s+|builtin\s+|noglob\s+|(?:/usr/bin/)?time\s+(?:-[A-Za-z]+\s+)*|stdbuf\s+(?:-[A-Za-z]+\s+\S+\s+)*|nice\s+(?:-n\s+\S+\s+)?)").unwrap()
});

static CONNECTOR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\s*(?:&&|\|\||;)\s*").unwrap()
});

/// `bash -lc '<cmd>'`, `zsh -c "<cmd>"`, `sh -c '<cmd>'` → captures the remainder
/// after `-c`; surrounding quotes are stripped in code (the `regex` crate has no
/// backreference support).
static SHELL_DASHC_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)^(?:ba|z)?sh\s+-[A-Za-z]*c\s+(.*)$"#).unwrap()
});

struct Detector {
    detector_type: &'static str,
    category: &'static str,
    command_patterns: &'static [&'static str],
    content_patterns: &'static [&'static str],
}

/// Pre-compiled regex cache for all detector patterns
static COMPILED_COMMAND_PATTERNS: Lazy<Vec<Vec<Regex>>> = Lazy::new(|| {
    DETECTORS.iter().map(|d| {
        d.command_patterns.iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    }).collect()
});

static COMPILED_CONTENT_PATTERNS: Lazy<Vec<Vec<Regex>>> = Lazy::new(|| {
    DETECTORS.iter().map(|d| {
        d.content_patterns.iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    }).collect()
});

static DETECTORS: &[Detector] = &[
    Detector { detector_type: "file-list", category: "shell",
        command_patterns: &[r"(?i)^(?:fd\b|find\b|rg\s+--files\b)"],
        content_patterns: &[r"(?m)^(?:\.{0,2}/|/|[\w.-]+/).+\.(?:rs|ts|tsx|js|jsx|py|go|java|rb|md|json|ya?ml|toml|txt|c|cc|cpp|h|hpp)$"] },
    Detector { detector_type: "structured-read", category: "shell",
        command_patterns: &[r"(?i)^(?:jq\b|yq\b|cat\s+.*\.(?:json|ya?ml|toml)\b)"],
        content_patterns: &[] },
    Detector { detector_type: "file-read", category: "shell",
        command_patterns: &[r"(?i)^(?:cat|bat|sed\s+-n|nl|head|tail)\b"],
        content_patterns: &[] },
    Detector { detector_type: "binary-read", category: "reverse",
        command_patterns: &[r"(?i)^(?:otool|nm|strings|dwarfdump|lipo|vtool|atos|dyld_info|size)\b"],
        content_patterns: &[] },
    Detector { detector_type: "macos-inspect", category: "reverse",
        command_patterns: &[r"(?i)^(?:sysctl|sw_vers|kmutil|codesign|plutil|spctl|memory_pressure|vm_stat|ioreg|system_profiler|lldb|kextstat)\b"],
        content_patterns: &[] },
    Detector { detector_type: "git-status", category: "git",
        command_patterns: &[r"(?i)^git\s+status\b"],
        content_patterns: &[r"(?m)^On branch ", r"(?m)^Changes (?:not staged|to be committed)", r"(?m)^Untracked files:"] },
    Detector { detector_type: "git-branch", category: "git",
        command_patterns: &[r"(?i)^git\s+(?:branch|checkout|switch)\b"],
        // `^\*\s+\S+$` (single token after the marker) targets `* main`, not
        // multi-word markdown bullets like `* buy milk`.
        content_patterns: &[r"(?m)^\*\s+\S+$", r"(?i)Switched to (?:a new )?branch", r#"(?i)Already on ['"][^'"]+['"]"#] },
    Detector { detector_type: "git-diff", category: "git",
        command_patterns: &[r"(?i)^git\s+(?:diff|show)\b"],
        content_patterns: &[r"(?m)^diff --git ", r"(?m)^@@\s+-\d+,\d+\s+\+\d+,\d+\s+@@"] },
    Detector { detector_type: "git-log", category: "git",
        command_patterns: &[r"(?i)^git\s+log\b"],
        content_patterns: &[r"(?m)^commit [0-9a-f]{7,40}", r"(?m)^Author: "] },
    Detector { detector_type: "make", category: "build",
        command_patterns: &[r"(?i)^make\b"],
        content_patterns: &[r"(?m)make\[\d+\]: (?:Entering|Leaving) directory", r"make: \*\*\* "] },
    Detector { detector_type: "terraform-plan", category: "infra",
        command_patterns: &[r"(?i)^terraform\s+plan\b"],
        content_patterns: &[r"Terraform will perform the following actions:", r"(?i)Plan: \d+ to add"] },
    Detector { detector_type: "tofu-plan", category: "infra",
        command_patterns: &[r"(?i)^(?:tofu|opentofu)\s+plan\b"],
        content_patterns: &[r"OpenTofu will perform the following actions:", r"(?i)Plan: \d+ to add"] },
    Detector { detector_type: "systemctl-status", category: "infra",
        command_patterns: &[r"(?i)^systemctl\s+status\b"],
        content_patterns: &[r"(?m)^\s*Loaded:\s+", r"(?m)^\s*Active:\s+", r"(?m)^●\s+\S+\.service"] },
    Detector { detector_type: "test-vitest", category: "test",
        command_patterns: &[r"(?i)^vitest\b", r"(?i)^npm\s+(?:run\s+)?test:vitest\b"],
        content_patterns: &[r"(?i)\bvitest\b", r"(?m)^ ✓ ", r"(?m)^ ❯ ", r"(?i)Test Files\s+\d+\s+(?:passed|failed)"] },
    Detector { detector_type: "test-jest", category: "test",
        command_patterns: &[r"(?i)^jest\b", r"(?i)^npm\s+(?:run\s+)?test\b"],
        content_patterns: &[r"(?i)Test Suites:\s+\d+", r"(?i)Tests:\s+\d+", r"(?m)^PASS\s+", r"(?m)^FAIL\s+"] },
    Detector { detector_type: "npm-script-build", category: "build",
        command_patterns: &[r"(?i)^npm\s+run\s+build\b", r"(?i)^npm\s+run\s+lint\b", r"(?i)^npm\s+run\s+typecheck\b"],
        content_patterns: &[r"(?i)compiled (?:successfully|with \d+ errors?)", r"(?i)vite v[\d.]+", r"(?i)Checking formatting\.\.\.", r"TS\d{4}:"] },
    Detector { detector_type: "conda", category: "package",
        command_patterns: &[r"(?i)^conda\b", r"(?i)^mamba\b"],
        content_patterns: &[r"Collecting package metadata", r"Solving environment", r"Preparing transaction", r"Executing transaction", r"PackagesNotFoundError", r"UnsatisfiableError"] },
    Detector { detector_type: "xcodebuild", category: "build",
        command_patterns: &[r"(?i)^xcodebuild\b"],
        content_patterns: &[r"\*\* BUILD FAILED \*\*", r"\*\* BUILD SUCCEEDED \*\*", r"(?m)^error: ", r"(?m)^Testing failed:"] },
    Detector { detector_type: "cmake", category: "build",
        command_patterns: &[r"(?i)^cmake\b"],
        content_patterns: &[r"(?m)^-- The [A-Z]+ compiler identification is", r"(?m)^CMake Error", r"(?m)^-- Build files have been written to:"] },
    Detector { detector_type: "swiftc", category: "build",
        command_patterns: &[r"(?i)^swiftc\b"],
        content_patterns: &[r"(?m)^error: ", r"(?m)^warning: ", r"link command failed with exit code", r"clang: error: linker command failed"] },
    Detector { detector_type: "cargo-build", category: "build",
        command_patterns: &[r"(?i)^cargo\s+(?:build|check|clippy|run)\b"],
        content_patterns: &[r"(?m)^\s*Compiling ", r"(?m)^\s*Finished ", r"error\[E\d+\]", r"(?m)^error(?:\[|:)", r"(?m)^warning: "] },
    Detector { detector_type: "swift-build", category: "build",
        command_patterns: &[r"(?i)^swift\s+(?:build|run|test)\b"],
        content_patterns: &[r"(?m)^Compiling ", r"Build complete!", r"(?m)^error: ", r"Compiling for"] },
    Detector { detector_type: "go-build", category: "build",
        command_patterns: &[r"(?i)^go\s+(?:build|run|vet|install)\b"],
        content_patterns: &[r"(?m)^#\s+\S", r"(?m)\.go:\d+:\d+: "] },
    Detector { detector_type: "clang-build", category: "build",
        command_patterns: &[r"(?i)^(?:clang|clang\+\+|gcc|g\+\+)\b"],
        content_patterns: &[r"(?m)^.+:\d+:\d+: (?:error|warning|note): ", r"linker command failed", r"ld: "] },
    Detector { detector_type: "xcrun", category: "build",
        command_patterns: &[r"(?i)^xcrun\b"],
        content_patterns: &[r"coremlcompiler: error:", r"(?m)^== Devices ==", r"App installed:", r"(?m)^error: "] },
    Detector { detector_type: "test-pytest", category: "test",
        command_patterns: &[r"(?i)^pytest\b", r"(?i)^python\s+-m\s+pytest\b"],
        content_patterns: &[r"(?i)=+\s+(?:\d+\s+)?(?:passed|failed|errors?)", r"(?m)^E\s+", r"(?m)^FAILED "] },
    Detector { detector_type: "test-cargo", category: "test",
        command_patterns: &[r"(?i)^cargo\s+(?:test|nextest)\b"],
        content_patterns: &[r"(?m)^running \d+ tests?", r"(?m)^test\s+[\w:.-]+\s+\.\.\.\s+(?:ok|FAILED|ignored)", r"(?i)test result:\s+(?:ok|FAILED)"] },
    Detector { detector_type: "test-go", category: "test",
        command_patterns: &[r"(?i)^go\s+test\b"],
        content_patterns: &[r"(?m)^(?:ok|FAIL)\s+[\w./-]+\s+[\d.]+s", r"(?m)^--- FAIL: ", r"(?m)^panic: "] },
    Detector { detector_type: "build-typescript", category: "build",
        command_patterns: &[r"(?i)^tsc\b", r"(?i)^npm\s+run\s+typecheck\b"],
        content_patterns: &[r"TS\d{4}:", r"(?i)error TS\d{4}"] },
    Detector { detector_type: "build-eslint", category: "build",
        command_patterns: &[r"(?i)^eslint\b", r"(?i)^npm\s+run\s+lint\b"],
        content_patterns: &[r"\s+\d+:\d+\s+(?:error|warning)\s+", r"✖\s+\d+\s+problems?"] },
    Detector { detector_type: "build-webpack", category: "build",
        command_patterns: &[r"(?i)^webpack\b", r"(?i)^npx\s+webpack\b", r"(?i)^npm\s+run\s+build:webpack\b"],
        content_patterns: &[r"webpack\s+\d", r"(?i)compiled (?:successfully|with \d+ errors?)", r"(?i)asset .+\.js"] },
    Detector { detector_type: "build-vite", category: "build",
        command_patterns: &[r"(?i)^vite\s+build\b", r"(?i)^npm\s+run\s+build\b", r"(?i)^pnpm\s+build\b"],
        content_patterns: &[r"(?i)vite v[\d.]+", r"(?i)✓ built in", r"(?i)transforming \(\d+\)"] },
    Detector { detector_type: "biome", category: "build",
        command_patterns: &[r"(?i)^biome\b", r"(?i)^npx\s+biome\b"],
        content_patterns: &[r"lint/[A-Za-z0-9/.-]+", r"(?i)Checked \d+ files? in"] },
    Detector { detector_type: "prettier", category: "build",
        command_patterns: &[r"(?i)^prettier\b", r"(?i)^npx\s+prettier\b"],
        content_patterns: &[r"(?m)^Checking formatting\.\.\.", r"(?i)Code style issues found"] },
    Detector { detector_type: "turbo", category: "build",
        command_patterns: &[r"(?i)^turbo\b", r"(?i)^npx\s+turbo\b"],
        content_patterns: &[r"(?m)^• Packages in scope:", r"(?m)^Tasks:\s+\d+\s+successful"] },
    Detector { detector_type: "nx", category: "build",
        command_patterns: &[r"(?i)^nx\b", r"(?i)^npx\s+nx\b"],
        content_patterns: &[r"(?m)^NX\s+", r"(?m)^> nx run "] },
    Detector { detector_type: "playwright", category: "test",
        command_patterns: &[r"(?i)^playwright\s+test\b", r"(?i)^npx\s+playwright\s+test\b"],
        content_patterns: &[r"(?i)Running \d+ tests? using \d+ workers?", r"(?m)^\s+\d+ failed"] },
    Detector { detector_type: "npm-install", category: "package",
        command_patterns: &[r"(?i)^(?:npm|pnpm|yarn)\s+(?:install|add|update)\b"],
        content_patterns: &[r"(?i)added \d+ packages", r"(?i)packages are looking for funding", r"(?i)audited \d+ packages"] },
    Detector { detector_type: "npm-audit", category: "package",
        command_patterns: &[r"(?i)^(?:npm|pnpm|yarn)\s+audit\b"],
        content_patterns: &[r"(?i)found \d+ vulnerabilities", r"(?i)\b(?:low|moderate|high|critical)\b"] },
    Detector { detector_type: "ruff", category: "build",
        command_patterns: &[r"(?i)^ruff\b", r"(?i)^uv\s+run\s+ruff\b"],
        content_patterns: &[r"(?m)^[\w./-]+\.py:\d+:\d+:\s+[A-Z]\d+", r"(?i)Found \d+ errors?\."] },
    Detector { detector_type: "mypy", category: "build",
        command_patterns: &[r"(?i)^mypy\b", r"(?i)^python\s+-m\s+mypy\b"],
        content_patterns: &[r"(?m)^[\w./-]+\.py:\d+:\s+error:", r"(?i)Found \d+ errors? in \d+ files?"] },
    Detector { detector_type: "pip", category: "package",
        command_patterns: &[r"(?i)^pip\s+(?:install|download|uninstall)\b", r"(?i)^python\s+-m\s+pip\b"],
        content_patterns: &[r"(?m)^Collecting ", r"(?m)^Successfully installed "] },
    // Generic Python execution (heredoc, -c, -m <module>, scripts, flags). Must come
    // AFTER pytest/mypy/pip so those more specific detectors win on equal-confidence
    // command matches; the `regex` crate has no look-around, so the exclusion is done
    // by detector ordering rather than a negative look-ahead.
    Detector { detector_type: "python-script", category: "build",
        command_patterns: &[r"(?i)^python(?:\d+(?:\.\d+)?)?\s", r"(?i)^python(?:\d+(?:\.\d+)?)?$"],
        content_patterns: &[r"Traceback \(most recent call last\):", r#"(?m)^File ".+", line \d+"#, r"AssertionError", r"ModuleNotFoundError", r"ImportError"] },
    Detector { detector_type: "uv-sync", category: "package",
        command_patterns: &[r"(?i)^uv\s+sync\b", r"(?i)^uv\s+pip\s+install\b"],
        content_patterns: &[r"(?m)^Resolved \d+ packages?", r"(?m)^Installed \d+ packages?"] },
    Detector { detector_type: "poetry-install", category: "package",
        command_patterns: &[r"(?i)^poetry\s+install\b"],
        content_patterns: &[r"(?m)^Installing dependencies from lock file", r"(?m)^Package operations:"] },
    Detector { detector_type: "golangci-lint", category: "build",
        command_patterns: &[r"(?i)^golangci-lint\b"],
        content_patterns: &[r"(?m)^[\w./-]+\.go:\d+:\d+:", r"(?m)^\d+ issues?:"] },
    Detector { detector_type: "bundle-install", category: "package",
        command_patterns: &[r"(?i)^bundle\s+install\b"],
        content_patterns: &[r"(?m)^Fetching gem metadata from ", r"(?m)^Bundle complete!"] },
    Detector { detector_type: "rubocop", category: "build",
        command_patterns: &[r"(?i)^rubocop\b", r"(?i)^bundle\s+exec\s+rubocop\b"],
        content_patterns: &[r"(?m)^Inspecting \d+ files", r"(?m)^[\w./-]+\.rb:\d+:\d+:\s+[A-Z]:"] },
    Detector { detector_type: "docker-ps", category: "docker",
        command_patterns: &[r"(?i)^docker\s+ps\b"],
        content_patterns: &[r"(?m)^CONTAINER ID\s+IMAGE\s+COMMAND"] },
    Detector { detector_type: "docker-logs", category: "docker",
        command_patterns: &[r"(?i)^docker\s+(?:compose\s+)?logs\b"],
        // Dropped the bare `\b(?:ERROR|WARN|INFO)\b` content pattern: it matched
        // any prose/web text containing those words and was a top source of
        // content-detection hijacking. Keep timestamp + container-attach signals.
        content_patterns: &[r"(?m)^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}", r"(?m)^Attaching to "] },
    Detector { detector_type: "docker-build", category: "docker",
        command_patterns: &[r"(?i)^docker\s+(?:build|buildx\s+build)\b", r"(?i)^(?:docker\s+compose|docker-compose)\s+build\b"],
        content_patterns: &[r"Successfully built \w+", r"exit code: \d+"] },
    Detector { detector_type: "aws", category: "cloud",
        command_patterns: &[r"(?i)^aws\b"],
        content_patterns: &[r"An error occurred \([A-Za-z0-9]+\) when calling", r"(?m)^(?:upload|download): "] },
    Detector { detector_type: "gcloud", category: "cloud",
        command_patterns: &[r"(?i)^gcloud\b"],
        content_patterns: &[r"(?m)^ERROR: \(gcloud\.", r"(?m)^Updated property \["] },
    Detector { detector_type: "ssh", category: "cloud",
        command_patterns: &[r"(?i)^ssh\b"],
        content_patterns: &[r"Permission denied \(", r"Host key verification failed", r"Connection timed out"] },
    Detector { detector_type: "rsync", category: "cloud",
        command_patterns: &[r"(?i)^rsync\b"],
        content_patterns: &[r"(?m)^sending incremental file list", r"(?m)^rsync error:"] },
    Detector { detector_type: "curl", category: "cloud",
        command_patterns: &[r"(?i)^curl\b"],
        content_patterns: &[r"curl: \(\d+\)", r"(?m)^HTTP/\d(?:\.\d)? \d{3}"] },
    Detector { detector_type: "wget", category: "cloud",
        command_patterns: &[r"(?i)^wget\b"],
        content_patterns: &[r"(?m)^--\d{4}-\d{2}-\d{2}", r"(?m)^ERROR \d{3}:"] },
    Detector { detector_type: "json-output", category: "generic",
        command_patterns: &[r"(?i)^jq\b", r"(?i)^cat\s+.*\.json\b"],
        content_patterns: &[r"^\s*[\[{][\s\S]*[\]}]\s*$"] },
    Detector { detector_type: "shell-ls", category: "shell",
        command_patterns: &[r"(?i)^ls(?:\s+-[A-Za-z]+)?\b"],
        content_patterns: &[r"(?m)^total \d+", r"(?m)^\S+\s+\S+\s+\d+\s+\w+\s+\d{1,2}\s+"] },
    Detector { detector_type: "shell-find", category: "shell",
        command_patterns: &[r"(?i)^find\b"],
        content_patterns: &[r"(?m)^(?:\.{1,2}|/|[\w.-]+/).+"] },
    Detector { detector_type: "shell-grep", category: "shell",
        command_patterns: &[r"(?i)^(?:grep|rg|ag)\b"],
        content_patterns: &[r"(?m)^[\w./-]+\.(?:ts|tsx|js|jsx|py|go|rs|java|rb|md|json|ya?ml|txt):\d*:", r"(?m)^[\w./-]+/[\w./-]+:\d*:"] },
    Detector { detector_type: "shell-ps", category: "shell",
        command_patterns: &[r"(?i)^ps\b"],
        content_patterns: &[r"(?m)^(?:USER\s+PID|\s*PID\s+)"] },
    Detector { detector_type: "shell-df", category: "shell",
        command_patterns: &[r"(?i)^df\b"],
        content_patterns: &[r"(?m)^Filesystem\s+.*Use%"] },
    Detector { detector_type: "shell-du", category: "shell",
        command_patterns: &[r"(?i)^du\b"],
        content_patterns: &[r"(?m)^\d+(?:\.\d+)?[KMGTP]?\s+\S+"] },
    Detector { detector_type: "error-stacktrace", category: "generic",
        command_patterns: &[],
        content_patterns: &[r"Traceback \(most recent call last\):", r"(?m)^\s+at\s+\S+\s+\(.+:\d+:\d+\)", r"(?m)^panic: ", r"(?m)^thread '[^']+' panicked at"] },
    Detector { detector_type: "generic-error", category: "generic",
        command_patterns: &[],
        content_patterns: &[r"Error:", r"Exception:", r"Traceback \(most recent call last\):"] },
];

/// Pick the first meaningful command line from a possibly multi-line command,
/// skipping leading comment (`#`) and blank lines.
fn first_command_line(text: &str) -> String {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        return trimmed.to_string();
    }
    text.trim().to_string()
}

/// Strip a single pair of matching surrounding quotes (`'…'` or `"…"`).
fn strip_matching_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'\'' || first == b'"') && first == last {
            return &s[1..s.len() - 1];
        }
    }
    s
}

fn strip_env_and_wrappers(cmd: &str) -> String {
    let mut cmd = cmd.trim().to_string();
    loop {
        let without_env = ENV_PREFIX_RE.replace(&cmd, "").to_string();
        let without_wrapper = SHELL_WRAPPER_RE.replace(&without_env, "").to_string();
        let next = without_wrapper.trim().to_string();
        if next == cmd {
            break;
        }
        cmd = next;
    }
    cmd
}

pub fn normalize_command_for_hint(command: &str) -> String {
    let stripped = command.trim().trim_start_matches("$ ").trim();
    let mut cmd = strip_env_and_wrappers(&first_command_line(stripped));

    // `bash -lc '<cmd>'` / `zsh -c "<cmd>"` → unwrap inner command once, then re-strip.
    if let Some(caps) = SHELL_DASHC_RE.captures(&cmd) {
        if let Some(rest) = caps.get(1) {
            let unquoted = strip_matching_quotes(rest.as_str().trim());
            let inner = first_command_line(unquoted);
            if !inner.is_empty() {
                cmd = strip_env_and_wrappers(&inner);
            }
        }
    }

    let segments: Vec<String> = CONNECTOR_RE
        .split(&cmd)
        .map(|segment| segment.trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect();

    for segment in &segments {
        // Rewrite a versioned/pathed python (e.g. `python3`, `/usr/bin/python3.11`)
        // to bare `python` first, so the prefix check matches segments that follow a
        // connector (the prefix pattern uses `python\b`, which misses `python3`).
        let candidate = PYTHON_PATH_RE.replace(segment.trim(), "python");
        if COMMAND_PREFIX_PATTERN.is_match(&candidate) {
            return candidate.into_owned();
        }
    }

    if let Some(first) = segments.first() {
        cmd = first.clone();
    }

    if PYTHON_PATH_RE.is_match(&cmd) {
        cmd = PYTHON_PATH_RE.replace(&cmd, "python").to_string();
    }

    cmd
}

/// True when `sed` is used to print/read lines, not in-place substitution.
pub fn is_sed_read_command(command: &str) -> bool {
    let cmd = normalize_command_for_hint(command);
    let lower = cmd.to_lowercase();
    if !lower.starts_with("sed") {
        return false;
    }
    if is_sed_substitution_command(&cmd) {
        return false;
    }
    if lower.contains(" -n") {
        return true;
    }
    static PRINT_SCRIPT_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?i)(?:['"][^'"]*\d+,\d*p[^'"]*['"]|['"][^'"]*\d+p[^'"]*['"]|-e\s+['"]?\d)"#).unwrap()
    });
    PRINT_SCRIPT_RE.is_match(&cmd)
}

fn is_sed_substitution_command(cmd: &str) -> bool {
    if cmd.contains("s/") || cmd.contains("s#") || cmd.contains("s|") {
        return true;
    }
    static SUBST_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r#"(?i)['"][^'"]*s[/|#]"#).unwrap()
    });
    SUBST_RE.is_match(cmd)
}

fn tokenize_shell_command(cmd: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut chars = cmd.chars().peekable();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let quoted = matches!(chars.peek(), Some('\'') | Some('"'));
        let mut token = String::new();
        if quoted {
            let quote = chars.next().unwrap();
            for c in chars.by_ref() {
                if c == quote {
                    break;
                }
                token.push(c);
            }
        } else {
            while let Some(c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                token.push(*c);
                chars.next();
            }
        }
        if !token.is_empty() {
            tokens.push(token);
        }
    }
    tokens
}

fn looks_like_search_path(token: &str) -> bool {
    token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token == "."
        || token == ".."
}

/// Extract search terms from rg/grep/ag commands for intent-aware compression.
///
/// Scans every line and connector/pipe segment of a (possibly multi-line) script,
/// not just the first command. This matters for heredoc-style scripts where the
/// heavy `rg`/`grep` runs on a later line while the first line is `echo`/`set`/`for`.
/// Regex patterns are reduced to their literal cores (e.g. `.{0,100}Foo.*` -> `Foo`)
/// so the keyword can actually substring-match output lines during truncation.
pub fn extract_intent_keywords(command: &str) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    for line in command.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let tokens = tokenize_shell_command(trimmed);
        for segment in split_command_segments(&tokens) {
            collect_segment_keywords(segment, &mut raw);
        }
    }

    let mut out: Vec<String> = Vec::new();
    for pattern in raw {
        for core in literal_cores(&pattern) {
            if core.len() >= 2 && !looks_like_search_path(&core) && !out.contains(&core) {
                out.push(core);
                if out.len() >= 4 {
                    return out;
                }
            }
        }
    }
    out
}

/// Split a tokenized command line into segments at shell connectors/pipes. Quotes are
/// already handled by the tokenizer, so a pipe inside a quoted regex stays intact.
fn split_command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (i, tok) in tokens.iter().enumerate() {
        if matches!(tok.as_str(), "&&" | "||" | ";" | "|" | "&") {
            if i > start {
                segments.push(&tokens[start..i]);
            }
            start = i + 1;
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

/// If a segment is an rg/grep/ag invocation, collect its raw search pattern(s).
fn collect_segment_keywords(segment: &[String], out: &mut Vec<String>) {
    // Skip leading `VAR=value` environment assignments.
    let mut start = 0;
    while start < segment.len() && is_env_assignment(&segment[start]) {
        start += 1;
    }
    let segment = &segment[start..];
    let Some(first) = segment.first() else {
        return;
    };
    let tool = last_path_component(first).to_lowercase();
    let found = match tool.as_str() {
        "rg" => extract_rg_keywords(segment),
        "grep" | "egrep" | "fgrep" => extract_grep_keywords(segment),
        "ag" => extract_ag_keywords(segment),
        _ => Vec::new(),
    };
    out.extend(found);
}

fn is_env_assignment(token: &str) -> bool {
    let mut chars = token.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    let mut saw_eq = false;
    for c in chars {
        if c == '=' {
            saw_eq = true;
            break;
        }
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    saw_eq
}

fn last_path_component(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// Reduce a (possibly regex) search pattern to literal cores usable for substring
/// matching: runs of word characters that contain at least one letter and are long
/// enough to be meaningful. A plain literal (no regex metacharacters) is kept whole.
fn literal_cores(pattern: &str) -> Vec<String> {
    const META: &str = r"\.^$|()[]{}*+?";
    if !pattern.chars().any(|c| META.contains(c)) {
        return vec![pattern.to_string()];
    }
    let mut cores = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, cores: &mut Vec<String>| {
        if cur.len() >= 3 && cur.chars().any(|c| c.is_ascii_alphabetic()) {
            cores.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for ch in pattern.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut cores);
        }
    }
    flush(&mut cur, &mut cores);
    cores
}

fn extract_rg_keywords(tokens: &[String]) -> Vec<String> {
    if tokens.iter().any(|t| t == "--files") {
        return Vec::new();
    }
    collect_search_pattern_tokens(tokens, "rg")
}

fn extract_grep_keywords(tokens: &[String]) -> Vec<String> {
    collect_search_pattern_tokens(tokens, "grep")
}

fn extract_ag_keywords(tokens: &[String]) -> Vec<String> {
    collect_search_pattern_tokens(tokens, "ag")
}

fn collect_search_pattern_tokens(tokens: &[String], tool: &str) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut i = 1;
    while i < tokens.len() {
        let token = &tokens[i];
        if token.starts_with('-') {
            let flag = token.as_str();
            if matches!(flag, "-e" | "--regexp" | "-f" | "--file") {
                i += 1;
                if i < tokens.len() && !tokens[i].starts_with('-') {
                    keywords.push(tokens[i].clone());
                }
            }
            i += 1;
            continue;
        }
        if keywords.is_empty() && !looks_like_search_path(token) {
            keywords.push(token.clone());
        }
        i += 1;
    }
    let _ = tool;
    keywords
}

fn detect_command_from_text(text: &str) -> Option<String> {
    for line in text.lines().take(4) {
        let trimmed = normalize_command_for_hint(line);
        if trimmed.is_empty() {
            continue;
        }
        if COMMAND_PREFIX_PATTERN.is_match(&trimmed) {
            return Some(trimmed);
        }
    }
    None
}

pub fn detect_command_type(text: &str, command: Option<&str>) -> CommandDetectionResult {
    let detected_command = command
        .map(normalize_command_for_hint)
        .filter(|c| !c.is_empty())
        .or_else(|| detect_command_from_text(text));

    let mut best: Option<CommandDetectionResult> = None;

    for (i, detector) in DETECTORS.iter().enumerate() {
        let command_matched = if let Some(ref cmd) = detected_command {
            let pattern_match = COMPILED_COMMAND_PATTERNS[i]
                .iter()
                .any(|re| re.is_match(cmd));
            if detector.detector_type == "file-read" {
                pattern_match || is_sed_read_command(cmd)
            } else {
                pattern_match
            }
        } else {
            false
        };

        let content_matches: usize = COMPILED_CONTENT_PATTERNS[i]
            .iter()
            .filter(|re| re.is_match(text))
            .count();

        if !command_matched && content_matches == 0 {
            continue;
        }

        let confidence = (if command_matched { 0.55 } else { 0.0 }
            + content_matches as f64 * 0.25)
            .min(1.0);

        // Confidence floor for content-only guesses: without a reliable command
        // hint, a classification must clear 0.5 — i.e. at least two distinct
        // signal patterns — to be trusted. A single broad pattern (a markdown
        // bullet, the word "critical", an ISO timestamp) is too weak and would
        // otherwise hijack unrelated prose/web/log output into an aggressive
        // filter. Principle: when unsure, leave it for the safe `unknown` path
        // (verbatim passthrough / char-budget only) rather than compress wrongly.
        if !command_matched && confidence < 0.5 {
            continue;
        }

        if best.as_ref().map_or(true, |b| confidence > b.confidence) {
            best = Some(CommandDetectionResult {
                command_type: detector.detector_type.to_string(),
                command: detected_command.clone(),
                confidence,
                category: detector.category.to_string(),
            });
        }
    }

    best.unwrap_or(CommandDetectionResult {
        command_type: "unknown".to_string(),
        command: detected_command.clone(),
        confidence: if detected_command.is_some() { 0.35 } else { 0.1 },
        category: "generic".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_command_strips_env_prefixes() {
        assert_eq!(
            normalize_command_for_hint("TMPDIR=/tmp/cache PYTHONPATH=/repo python -m pytest tests"),
            "python -m pytest tests"
        );
    }

    #[test]
    fn test_normalize_command_strips_shell_connectors() {
        assert_eq!(
            normalize_command_for_hint("cd repo && rg --files src"),
            "rg --files src"
        );
    }

    #[test]
    fn test_detect_command_type_uses_normalized_command() {
        let detection = detect_command_type(
            "some output",
            Some("TMPDIR=/tmp/cache cargo test --package toksqz"),
        );
        assert_eq!(detection.command_type, "test-cargo");
    }

    #[test]
    fn test_normalize_command_rewrites_python_path_binary() {
        assert_eq!(
            normalize_command_for_hint("/Users/demo/miniconda3/envs/x/bin/python -m pytest tests"),
            "python -m pytest tests"
        );
    }

    #[test]
    fn test_is_sed_read_command_detects_print_scripts() {
        assert!(is_sed_read_command("sed -n '1,240p' src/main.rs"));
        assert!(is_sed_read_command("sed '120,180p' README.md"));
        assert!(!is_sed_read_command("sed -i '' 's/foo/bar/' src/main.rs"));
        assert!(!is_sed_read_command("sed 's/old/new/g' file.txt"));
    }

    #[test]
    fn test_extract_intent_keywords_from_rg() {
        assert_eq!(
            extract_intent_keywords("cd repo && rg -n target_symbol src"),
            vec!["target_symbol".to_string()]
        );
        assert!(extract_intent_keywords("rg --files src").is_empty());
        assert_eq!(
            extract_intent_keywords("rg \"parse_config\" src/compression"),
            vec!["parse_config".to_string()]
        );
    }

    #[test]
    fn test_sed_print_script_routes_to_file_read() {
        let detection = detect_command_type("line one\nline two", Some("sed '1,240p' src/mod.rs"));
        assert_eq!(detection.command_type, "file-read");
    }

    #[test]
    fn test_sed_substitution_does_not_route_to_file_read() {
        let detection = detect_command_type("done", Some("sed -i '' 's/foo/bar/' src/mod.rs"));
        assert_ne!(detection.command_type, "file-read");
    }

    #[test]
    fn test_normalize_unwraps_bash_dash_c() {
        assert_eq!(
            normalize_command_for_hint("bash -lc 'cd repo && rg -n foo src'"),
            "rg -n foo src"
        );
        assert_eq!(
            normalize_command_for_hint("zsh -c \"cargo build --release\""),
            "cargo build --release"
        );
    }

    #[test]
    fn test_normalize_strips_usr_bin_time_wrapper() {
        assert_eq!(
            normalize_command_for_hint("/usr/bin/time -lp cargo build --release"),
            "cargo build --release"
        );
    }

    #[test]
    fn test_normalize_skips_leading_comment_lines() {
        assert_eq!(
            normalize_command_for_hint("# find the ANE driver\nsysctl -a"),
            "sysctl -a"
        );
    }

    #[test]
    fn test_detect_cargo_build() {
        let detection = detect_command_type(
            "   Compiling toksqz v0.1.0\n    Finished dev [unoptimized]",
            Some("cargo build"),
        );
        assert_eq!(detection.command_type, "cargo-build");
    }

    #[test]
    fn test_detect_swift_and_go_build() {
        assert_eq!(
            detect_command_type("Build complete!", Some("swift build")).command_type,
            "swift-build"
        );
        assert_eq!(
            detect_command_type("# command-line-arguments", Some("go build ./...")).command_type,
            "go-build"
        );
    }

    #[test]
    fn test_detect_clang_build() {
        let detection = detect_command_type(
            "foo.c:10:5: error: use of undeclared identifier 'bar'",
            Some("clang -c foo.c -o foo.o"),
        );
        assert_eq!(detection.command_type, "clang-build");
    }

    #[test]
    fn test_detect_macos_inspect() {
        assert_eq!(
            detect_command_type("kern.osversion: 24F74", Some("sysctl -a")).command_type,
            "macos-inspect"
        );
        assert_eq!(
            detect_command_type("Executable=/bin/ls", Some("codesign -dv /bin/ls")).command_type,
            "macos-inspect"
        );
    }

    #[test]
    fn test_dyld_info_routes_to_binary_read() {
        assert_eq!(
            detect_command_type("0x1000 _main", Some("dyld_info -exports /bin/ls")).command_type,
            "binary-read"
        );
    }

    #[test]
    fn test_python_script_detects_common_forms() {
        for cmd in [
            "python foo.py",
            "python3 foo.py",
            "python -u scripts/run.py",
            "python -c 'print(1)'",
            "python3 -c \"import sys\"",
            "python - <<'PY'",
            "python <<'PY'",
            "python -m py_compile a.py",
            "python -m compileall src",
            "python -m pymss.cli --check",
            "cd repo && python3 - <<'PY'",
        ] {
            assert_eq!(
                detect_command_type("", Some(cmd)).command_type,
                "python-script",
                "expected python-script for: {cmd}"
            );
        }
    }

    #[test]
    fn test_intent_keywords_from_multiline_script() {
        let cmd = "# inspect XPC\necho \"=== scan ===\"\nrg -a -o 'TopLevelGraphEncodingNode' /usr/lib/foo";
        assert_eq!(
            extract_intent_keywords(cmd),
            vec!["TopLevelGraphEncodingNode".to_string()],
            "rg pattern on a later line of a multi-line script must be captured"
        );
    }

    #[test]
    fn test_intent_keywords_extract_literal_core_from_regex() {
        let cmd = "rg -a -o '.{0,100}TopLevelGraphEncodingNode.{0,100}' bin";
        assert_eq!(
            extract_intent_keywords(cmd),
            vec!["TopLevelGraphEncodingNode".to_string()],
            "regex quantifiers/anchors must be stripped to a usable literal core"
        );
    }

    #[test]
    fn test_intent_keywords_split_alternation_cores() {
        let cmd = "rg -n 'winreg|cfg_windows' src";
        assert_eq!(
            extract_intent_keywords(cmd),
            vec!["winreg".to_string(), "cfg_windows".to_string()]
        );
    }

    #[test]
    fn test_intent_keywords_from_piped_grep() {
        let cmd = "cat huge.log | grep needle_token";
        assert_eq!(
            extract_intent_keywords(cmd),
            vec!["needle_token".to_string()],
            "grep after a pipe must contribute intent keywords"
        );
    }

    #[test]
    fn test_intent_keywords_pipe_inside_quoted_pattern_not_split() {
        let cmd = "rg 'foo|bar' src";
        assert_eq!(
            extract_intent_keywords(cmd),
            vec!["foo".to_string(), "bar".to_string()],
            "a quoted alternation is one pattern, split into cores, not a shell pipe"
        );
    }

    #[test]
    fn test_intent_keywords_skip_env_prefix_in_segment() {
        let cmd = "TMPDIR=/tmp rg -n target_sym src";
        assert_eq!(extract_intent_keywords(cmd), vec!["target_sym".to_string()]);
    }

    #[test]
    fn test_python_subtools_still_route_to_specific_detectors() {
        assert_eq!(
            detect_command_type("", Some("python -m pytest tests")).command_type,
            "test-pytest"
        );
        assert_eq!(
            detect_command_type("", Some("python -m mypy src")).command_type,
            "mypy"
        );
        assert_eq!(
            detect_command_type("", Some("python -m pip install requests")).command_type,
            "pip"
        );
    }
}
