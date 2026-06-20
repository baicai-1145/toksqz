use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;

struct CavemanRule {
    pattern: Regex,
    replacement: String,
    context: String,       // "all", "user", "assistant", "system"
    min_intensity: Intensity,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Intensity {
    Lite,
    Full,
    Ultra,
}

impl Intensity {
    fn from_str(s: &str) -> Self {
        match s {
            "ultra" => Intensity::Ultra,
            "full" => Intensity::Full,
            _ => Intensity::Lite,
        }
    }
}

fn parse_intensity(s: &str) -> Intensity {
    Intensity::from_str(s)
}

#[derive(Deserialize)]
struct RawCavemanRule {
    pattern: String,
    replacement: String,
    #[allow(dead_code)]
    context: String,
    #[allow(dead_code)]
    category: String,
    #[serde(rename = "minIntensity")]
    min_intensity: String,
    #[serde(default)]
    flags: Option<String>,
}

static RULES: Lazy<Vec<CavemanRule>> = Lazy::new(|| {
    let raw_json = include_str!("../../assets/caveman_rules_en.json");
    let raw_rules: Vec<RawCavemanRule> = serde_json::from_str(raw_json)
        .expect("Failed to parse caveman rules JSON");

    raw_rules.into_iter().filter_map(|raw| {
        let pattern_str = if raw.flags.as_deref() == Some("g") {
            // Already has global flag, just use the pattern
            raw.pattern.clone()
        } else {
            raw.pattern.clone()
        };

        let re = Regex::new(&pattern_str).ok()?;

        Some(CavemanRule {
            pattern: re,
            replacement: raw.replacement,
            context: raw.context,
            min_intensity: parse_intensity(&raw.min_intensity),
        })
    }).collect()
});

pub fn load_rules() {
    Lazy::force(&RULES);
}

pub fn compress(text: &str, role: &str, level: &str) -> String {
    let level_intensity = parse_intensity(level);
    let mut result = text.to_string();

    for rule in RULES.iter() {
        // Check intensity: rule applies if its min_intensity <= current level
        if rule.min_intensity > level_intensity {
            continue;
        }

        // Check context: "all" matches everything, otherwise must match role
        if rule.context != "all" && rule.context != role {
            continue;
        }

        // Skip if no match (avoids allocation)
        if !rule.pattern.is_match(&result) {
            continue;
        }

        result = rule.pattern.replace_all(&result, rule.replacement.as_str()).to_string();
    }

    // Clean up multiple spaces
    static MULTI_SPACE: Lazy<Regex> = Lazy::new(|| Regex::new(r"  +").unwrap());
    if MULTI_SPACE.is_match(&result) {
        result = MULTI_SPACE.replace_all(&result, " ").to_string();
    }

    // Clean up leading/trailing whitespace per line
    result = result.lines()
        .map(|l| l.trim())
        .collect::<Vec<_>>()
        .join("\n");

    result
}
