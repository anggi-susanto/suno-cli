use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;

#[derive(Serialize)]
struct Entry {
    stage: &'static str,
    start_ms: f64,
    duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<Attempt>,
}

#[derive(Serialize, Clone, Copy)]
struct Attempt {
    number: u8,
    mode: &'static str,
    provider: u8,
    outcome: &'static str,
}

struct Recorder {
    start: Instant,
    entries: Mutex<Vec<Entry>>,
}

static RECORDER: OnceLock<Recorder> = OnceLock::new();

pub fn enable(start: Instant) {
    let _ = RECORDER.set(Recorder {
        start,
        entries: Mutex::new(Vec::new()),
    });
}

pub struct Span {
    stage: &'static str,
    start: Option<Instant>,
    attempt: Option<Attempt>,
}

pub fn span(stage: &'static str) -> Span {
    Span {
        stage,
        start: RECORDER.get().map(|_| Instant::now()),
        attempt: None,
    }
}

impl Span {
    pub fn attempt(mut self, number: u8, mode: &'static str, provider: u8) -> Self {
        self.attempt = Some(Attempt {
            number,
            mode,
            provider,
            outcome: "cancelled",
        });
        self
    }

    pub fn outcome(&mut self, outcome: &'static str) {
        if let Some(attempt) = &mut self.attempt {
            attempt.outcome = outcome;
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        if let (Some(recorder), Some(start)) = (RECORDER.get(), self.start) {
            recorder.entries.lock().unwrap().push(Entry {
                stage: self.stage,
                start_ms: start.duration_since(recorder.start).as_secs_f64() * 1000.0,
                duration_ms: start.elapsed().as_secs_f64() * 1000.0,
                attempt: self.attempt,
            });
        }
    }
}

pub fn snapshot() -> Option<serde_json::Value> {
    let recorder = RECORDER.get()?;
    let mut entries = recorder.entries.lock().unwrap();
    entries.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
    Some(serde_json::json!({
        "total_ms": recorder.start.elapsed().as_secs_f64() * 1000.0,
        "stages": *entries,
    }))
}

pub fn print(report: &serde_json::Value, json: bool) {
    use std::io::Write;
    let mut stderr = std::io::stderr().lock();
    if json {
        let _ = writeln!(stderr, "{}", serde_json::json!({"timings": report}));
    } else {
        if let Some(entries) = report["stages"].as_array() {
            for entry in entries {
                if let Some(attempt) = entry.get("attempt") {
                    let _ = writeln!(stderr, "[timings] captcha attempt: {attempt}");
                }
                let _ = writeln!(
                    stderr,
                    "[timings] {}: {:.2} ms",
                    entry["stage"].as_str().unwrap_or(""),
                    entry["duration_ms"].as_f64().unwrap_or(0.0)
                );
            }
        }
        let _ = writeln!(
            stderr,
            "[timings] total: {:.2} ms",
            report["total_ms"].as_f64().unwrap_or(0.0)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempt_records_only_safe_metadata_and_static_outcome() {
        let mut timing = span("captcha.attempt").attempt(2, "headless", 1);
        timing.outcome("config_error");
        let entry = Entry {
            stage: timing.stage,
            start_ms: 0.0,
            duration_ms: 1.0,
            attempt: timing.attempt,
        };
        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(
            value["attempt"],
            serde_json::json!({
                "number": 2, "mode": "headless", "provider": 1, "outcome": "config_error"
            })
        );
        assert_eq!(value.as_object().unwrap().len(), 4);
    }

    #[test]
    fn unfinished_attempt_is_marked_cancelled() {
        let timing = span("captcha.attempt").attempt(1, "headed", 2);
        assert_eq!(timing.attempt.unwrap().outcome, "cancelled");
    }
}
