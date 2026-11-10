//! Percentile and system helpers for the measurement harness.

use std::collections::HashMap;
use std::fs;

/// Distribution summary, in milliseconds from microsecond inputs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Dist {
    pub count: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

pub fn dist(values: &[u64]) -> Dist {
    if values.is_empty() {
        return Dist::default();
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let at = |p: f64| {
        let index = ((p * sorted.len() as f64).ceil() as usize).saturating_sub(1);
        sorted[index.min(sorted.len() - 1)] as f64 / 1000.0
    };
    let sum: u64 = sorted.iter().sum();
    Dist {
        count: sorted.len(),
        mean_ms: sum as f64 / sorted.len() as f64 / 1000.0,
        p50_ms: at(0.50),
        p95_ms: at(0.95),
        p99_ms: at(0.99),
        max_ms: sorted[sorted.len() - 1] as f64 / 1000.0,
    }
}

pub fn dist_json(d: Dist) -> String {
    format!(
        r#"{{"count":{},"mean_ms":{:.3},"p50_ms":{:.3},"p95_ms":{:.3},"p99_ms":{:.3},"max_ms":{:.3}}}"#,
        d.count, d.mean_ms, d.p50_ms, d.p95_ms, d.p99_ms, d.max_ms
    )
}

/// RSS of this process in kB from `/proc/self/status`.
pub fn rss_kb() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:").map(str::to_string))
        })
        .and_then(|v| v.split_whitespace().next().map(str::to_string))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Anonymous memory currently backed by transparent huge pages, in KiB. With
/// THP on, khugepaged's periodic collapse fills holes and raises RSS in steps.
pub fn anon_huge_kb() -> u64 {
    fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("AnonHugePages:").map(str::to_string))
        })
        .and_then(|v| v.split_whitespace().next().map(str::to_string))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Accumulated user+system CPU time of this process in seconds.
pub fn cpu_seconds() -> f64 {
    let stat = fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Field 14/15 (1-indexed) are utime/stime; the comm field can contain spaces.
    let after = stat.rsplit(')').next().unwrap_or("");
    let fields: Vec<&str> = after.split_whitespace().collect();
    let parse = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (parse(11) + parse(12)) / 100.0
}

/// CPU seconds consumed so far by each thread of this process, keyed by thread
/// name (comm), summing threads that share a name (lavapipe's workers).
pub fn thread_cpu_seconds() -> HashMap<String, f64> {
    let mut totals: HashMap<String, f64> = HashMap::new();
    let Ok(dir) = fs::read_dir("/proc/self/task") else {
        return totals;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let comm = fs::read_to_string(path.join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let stat = fs::read_to_string(path.join("stat")).unwrap_or_default();
        *totals.entry(comm).or_default() += stat_cpu_seconds(&stat);
    }
    totals
}

/// CPU seconds of an arbitrary process, 0 if it has exited.
pub fn pid_cpu_seconds(pid: u32) -> f64 {
    stat_cpu_seconds(&fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default())
}

fn stat_cpu_seconds(stat: &str) -> f64 {
    let after = stat.rsplit(')').next().unwrap_or("");
    let fields: Vec<&str> = after.split_whitespace().collect();
    let parse = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (parse(11) + parse(12)) / 100.0
}

/// CPU percent per thread name between two `thread_cpu_seconds` readings.
pub fn thread_cpu_json(start: &HashMap<String, f64>, end: &HashMap<String, f64>, elapsed: f64) -> String {
    let mut rows: Vec<(String, f64)> = end
        .iter()
        .map(|(name, seconds)| {
            let start_value = start.get(name).copied().unwrap_or(0.0);
            (name.clone(), (seconds - start_value).max(0.0) / elapsed * 100.0)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    let body: Vec<String> = rows
        .iter()
        .filter(|(_, percent)| *percent >= 0.05)
        .map(|(name, percent)| format!("{name:?}:{percent:.1}"))
        .collect();
    format!("{{{}}}", body.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dist_reports_nearest_rank_percentiles() {
        let values: Vec<u64> = (1..=100).map(|v| v * 1000).collect();
        let d = dist(&values);
        assert_eq!(d.count, 100);
        assert_eq!(d.p50_ms, 50.0);
        assert_eq!(d.p95_ms, 95.0);
        assert_eq!(d.p99_ms, 99.0);
        assert_eq!(d.max_ms, 100.0);
    }

    #[test]
    fn dist_handles_empty() {
        assert_eq!(dist(&[]).count, 0);
    }
}
