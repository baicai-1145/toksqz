use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;
use once_cell::sync::Lazy;
use serde::Serialize;

/// Per-request compression detail for response headers.
#[derive(Default, Clone)]
pub struct RequestStats {
    pub filters_applied: Vec<String>,
    pub per_command: Vec<CommandStats>,
}

#[derive(Clone)]
pub struct CommandStats {
    pub command_type: String,
    pub filter_id: String,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
}

/// Global cumulative statistics.
static GLOBAL_STATS: Lazy<RwLock<GlobalStats>> = Lazy::new(|| {
    RwLock::new(GlobalStats::default())
});

static TOTAL_REQUESTS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ORIGINAL_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_COMPRESSED_TOKENS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_SAVED_TOKENS: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct GlobalStats {
    filter_hits: HashMap<String, usize>,
    command_type_hits: HashMap<String, usize>,
    command_type_saved: HashMap<String, usize>,
}

/// Record stats for a single compressed message.
pub fn record_message(filter_id: &str, command_type: &str, original: usize, compressed: usize) {
    TOTAL_REQUESTS.fetch_add(1, Ordering::Relaxed);
    TOTAL_ORIGINAL_TOKENS.fetch_add(original, Ordering::Relaxed);
    TOTAL_COMPRESSED_TOKENS.fetch_add(compressed, Ordering::Relaxed);
    let saved = original.saturating_sub(compressed);
    TOTAL_SAVED_TOKENS.fetch_add(saved, Ordering::Relaxed);
    record_time_series(original, compressed);

    if let Ok(mut stats) = GLOBAL_STATS.write() {
        *stats.filter_hits.entry(filter_id.to_string()).or_insert(0) += 1;
        *stats.command_type_hits.entry(command_type.to_string()).or_insert(0) += 1;
        *stats.command_type_saved.entry(command_type.to_string()).or_insert(0) += saved;
    }
}

/// Get summary statistics for the `/stats` endpoint.
pub fn get_summary() -> StatsSummary {
    let total_requests = TOTAL_REQUESTS.load(Ordering::Relaxed);
    let total_original = TOTAL_ORIGINAL_TOKENS.load(Ordering::Relaxed);
    let total_compressed = TOTAL_COMPRESSED_TOKENS.load(Ordering::Relaxed);
    let total_saved = TOTAL_SAVED_TOKENS.load(Ordering::Relaxed);
    let avg_savings_pct = if total_original > 0 {
        (total_saved as f64 / total_original as f64) * 100.0
    } else {
        0.0
    };

    let (filter_hits, command_hits) = if let Ok(stats) = GLOBAL_STATS.read() {
        let mut fh: Vec<(String, usize)> = stats.filter_hits.iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        fh.sort_by(|a, b| b.1.cmp(&a.1));

        let mut ch: Vec<CommandHit> = stats.command_type_hits.iter()
            .map(|(k, v)| {
                let saved = stats.command_type_saved.get(k).copied().unwrap_or(0);
                CommandHit {
                    command_type: k.clone(),
                    hits: *v,
                    saved_tokens: saved,
                }
            })
            .collect();
        ch.sort_by(|a, b| b.saved_tokens.cmp(&a.saved_tokens));

        (fh, ch)
    } else {
        (Vec::new(), Vec::new())
    };

    StatsSummary {
        total_requests,
        total_original_tokens: total_original,
        total_compressed_tokens: total_compressed,
        total_saved_tokens: total_saved,
        avg_savings_pct,
        filter_hits,
        command_hits,
    }
}

#[derive(Serialize)]
pub struct StatsSummary {
    pub total_requests: usize,
    pub total_original_tokens: usize,
    pub total_compressed_tokens: usize,
    pub total_saved_tokens: usize,
    pub avg_savings_pct: f64,
    pub filter_hits: Vec<(String, usize)>,
    pub command_hits: Vec<CommandHit>,
}

#[derive(Serialize)]
pub struct CommandHit {
    pub command_type: String,
    pub hits: usize,
    pub saved_tokens: usize,
}

#[derive(Serialize)]
pub struct TimePoint {
    pub label: String,
    pub requests: usize,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub saved_tokens: usize,
}

#[derive(Serialize)]
pub struct TimeSeriesData {
    pub monthly: Vec<TimePoint>,
    pub daily: Vec<TimePoint>,
    pub hourly: Vec<TimePoint>,
}

// ─── Time-series tracking ────────────────────────────────────────────────

#[derive(Default)]
struct TsBucket {
    requests: usize,
    original: usize,
    compressed: usize,
}

#[derive(Default)]
struct TimeSeriesStore {
    monthly: HashMap<String, TsBucket>,   // "YYYY-MM"
    daily: HashMap<String, TsBucket>,     // "YYYY-MM-DD"
    hourly: HashMap<String, TsBucket>,    // "YYYY-MM-DDTHH"
}

static TIME_SERIES: Lazy<RwLock<TimeSeriesStore>> = Lazy::new(|| {
    RwLock::new(TimeSeriesStore::default())
});

fn record_time_series(original: usize, compressed: usize) {
    if let Ok(mut ts) = TIME_SERIES.write() {
        let now = chrono_now();
        let month = now[..7].to_string();    // "YYYY-MM"
        let day = now[..10].to_string();     // "YYYY-MM-DD"
        let hour = format!("{}T{}", &now[..10], &now[11..13]); // "YYYY-MM-DDTHH"

        let m = ts.monthly.entry(month).or_default();
        m.requests += 1; m.original += original; m.compressed += compressed;

        let d = ts.daily.entry(day).or_default();
        d.requests += 1; d.original += original; d.compressed += compressed;

        let h = ts.hourly.entry(hour).or_default();
        h.requests += 1; h.original += original; h.compressed += compressed;
    }
}

/// Simple UTC timestamp without chrono dependency: "YYYY-MM-DDTHH:MM:SS"
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;
    let sec = time_of_day % 60;

    // Civil date from days since epoch (Howard Hinnant algorithm)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, m, d, hour, min, sec)
}

/// Get time series data for the dashboard.
pub fn get_time_series() -> TimeSeriesData {
    let ts = TIME_SERIES.read().unwrap();

    let mut monthly: Vec<TimePoint> = ts.monthly.iter().map(|(k, v)| TimePoint {
        label: k.clone(), requests: v.requests,
        original_tokens: v.original, compressed_tokens: v.compressed,
        saved_tokens: v.original.saturating_sub(v.compressed),
    }).collect();
    monthly.sort_by(|a, b| a.label.cmp(&b.label));

    let mut daily: Vec<TimePoint> = ts.daily.iter().map(|(k, v)| TimePoint {
        label: k.clone(), requests: v.requests,
        original_tokens: v.original, compressed_tokens: v.compressed,
        saved_tokens: v.original.saturating_sub(v.compressed),
    }).collect();
    daily.sort_by(|a, b| a.label.cmp(&b.label));

    let mut hourly: Vec<TimePoint> = ts.hourly.iter().map(|(k, v)| TimePoint {
        label: k.clone(), requests: v.requests,
        original_tokens: v.original, compressed_tokens: v.compressed,
        saved_tokens: v.original.saturating_sub(v.compressed),
    }).collect();
    hourly.sort_by(|a, b| a.label.cmp(&b.label));

    TimeSeriesData { monthly, daily, hourly }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_summary() {
        // Note: this test uses global state, so it might interact with other tests
        // in a parallel test run. For isolated testing, just verify the API works.
        record_message("git-status", "git-status", 100, 30);
        let summary = get_summary();
        assert!(summary.total_requests > 0);
        assert!(summary.total_saved_tokens > 0);
    }

    #[test]
    fn test_empty_summary() {
        let summary = get_summary();
        // avg_savings_pct should be a valid number
        assert!(summary.avg_savings_pct.is_finite());
    }
}
