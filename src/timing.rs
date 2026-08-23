//! Per-stage timing, so it is possible to see where a long run spends its time.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, serde::Serialize)]
pub struct StageTiming {
    pub stage: &'static str,
    pub seconds: f64,
    /// Items handled, when the stage counts items (files, images, comparisons).
    pub items: Option<u64>,
    /// Bytes read, when the stage reads bytes.
    pub bytes: Option<u64>,
    /// Human-readable rate, for example `"820.4 MB/s"` or `"128 images/s"`.
    pub throughput: Option<String>,
}

#[derive(Debug, Default)]
pub struct Timings {
    stages: Mutex<Vec<StageTiming>>,
}

impl Timings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run `body`, record how long it took, and return its value.
    ///
    /// `body` returns the value plus optional item and byte counts, so a stage
    /// can report throughput without the caller counting anything twice.
    pub fn measure<T>(
        &self,
        stage: &'static str,
        body: impl FnOnce() -> (T, Option<u64>, Option<u64>),
    ) -> T {
        let started = Instant::now();
        let (value, items, bytes) = body();
        self.record(stage, started.elapsed(), items, bytes);
        value
    }

    pub fn record(
        &self,
        stage: &'static str,
        elapsed: Duration,
        items: Option<u64>,
        bytes: Option<u64>,
    ) {
        let seconds = elapsed.as_secs_f64();
        let unit = item_unit(stage);
        let throughput = if seconds <= 0.0 {
            None
        } else if let Some(bytes) = bytes {
            Some(format!(
                "{:.1} MB/s",
                bytes as f64 / seconds / (1024.0 * 1024.0)
            ))
        } else {
            items.map(|items| format!("{:.0} {unit}/s", items as f64 / seconds))
        };

        self.stages
            .lock()
            .expect("timings mutex poisoned")
            .push(StageTiming {
                stage,
                seconds,
                items,
                bytes,
                throughput,
            });
    }

    pub fn snapshot(&self) -> Vec<StageTiming> {
        self.stages.lock().expect("timings mutex poisoned").clone()
    }
}

fn item_unit(stage: &str) -> &'static str {
    match stage {
        "analyze" | "fingerprint" => "images",
        "match" => "comparisons",
        _ => "files",
    }
}
