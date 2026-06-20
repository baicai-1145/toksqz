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
    "ssh", "rsync", "curl", "wget", "ls", "find", "grep", "rg",
    "ag", "ps", "df", "du",
];

static COMMAND_PREFIX_PATTERN: Lazy<Regex> = Lazy::new(|| {
    let pattern = format!("^(?:{})\\b", COMMAND_PREFIXES.join("|"));
    Regex::new(&pattern).unwrap()
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
    Detector { detector_type: "git-status", category: "git",
        command_patterns: &[r"(?i)^git\s+status\b"],
        content_patterns: &[r"(?m)^On branch ", r"(?m)^Changes (?:not staged|to be committed)", r"(?m)^Untracked files:"] },
    Detector { detector_type: "git-branch", category: "git",
        command_patterns: &[r"(?i)^git\s+(?:branch|checkout|switch)\b"],
        content_patterns: &[r"(?m)^\*\s+\S+", r"(?i)Switched to (?:a new )?branch", r#"(?i)Already on ['"][^'"]+['"]"#] },
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
        content_patterns: &[r"(?m)^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}", r"\b(?:ERROR|WARN|INFO)\b", r"(?m)^Attaching to "] },
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

fn detect_command_from_text(text: &str) -> Option<String> {
    for line in text.lines().take(4) {
        let trimmed = line.trim().trim_start_matches("$ ");
        if trimmed.is_empty() {
            continue;
        }
        if COMMAND_PREFIX_PATTERN.is_match(trimmed) {
            return Some(trimmed.to_string());
        }
    }
    None
}

pub fn detect_command_type(text: &str, command: Option<&str>) -> CommandDetectionResult {
    let detected_command = command
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .or_else(|| detect_command_from_text(text));

    let mut best: Option<CommandDetectionResult> = None;

    for (i, detector) in DETECTORS.iter().enumerate() {
        let command_matched = if let Some(ref cmd) = detected_command {
            COMPILED_COMMAND_PATTERNS[i].iter().any(|re| re.is_match(cmd))
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
