use std::sync::Arc;

/// A single progress update from a long-running core operation.
/// `total` may be 0 when the total is unknown (show spinner instead of %).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub done: u64,
    pub total: u64,
    pub label: String,
}

/// Callback invoked from worker threads as work proceeds.
/// Must be cheap and non-blocking; failures are the caller's problem
/// (e.g. send on a channel and ignore a full/disconnected receiver).
#[derive(Clone)]
pub struct ProgressReporter {
    inner: Arc<dyn Fn(Progress) + Send + Sync>,
}

impl ProgressReporter {
    pub fn new(f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Self { inner: Arc::new(f) }
    }

    pub fn report(&self, done: u64, total: u64, label: impl Into<String>) {
        (self.inner)(Progress {
            done,
            total,
            label: label.into(),
        });
    }
}

impl std::fmt::Debug for ProgressReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProgressReporter(..)")
    }
}
