//! Reporting what a refresh is doing, so a slow one doesn't look stuck.

use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// `done` of `total` searches have finished, finding `found` PRs so far.
    Searching {
        done: usize,
        total: usize,
        found: usize,
    },
    /// Review and timeline details for `done` of `total` PRs.
    Details { done: usize, total: usize },
    /// A request failed and is being retried.
    Retrying {
        attempt: u32,
        max: u32,
        reason: String,
    },
}

impl Progress {
    /// A short description, e.g. `details 50/120`.
    pub fn describe(&self) -> String {
        match self {
            Progress::Searching { done, total, found } => {
                format!("searching {done}/{total} ({found} PRs so far)")
            }
            Progress::Details { done, total } => format!("details {done}/{total}"),
            Progress::Retrying {
                attempt,
                max,
                reason,
            } => format!("{reason}; retrying ({attempt}/{max})"),
        }
    }
}

/// Without progress for this long, say so, so a stalled refresh doesn't look
/// like a slow one.
pub const STALL_SECS: i64 = 15;

/// `9s`, `1m 05s`
pub fn elapsed(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// Where progress goes; cheap to clone, and a no-op when nobody's listening.
#[derive(Clone, Default)]
pub struct Reporter(Option<Arc<dyn Fn(Progress) + Send + Sync>>);

impl Reporter {
    pub fn new(f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Reporter(Some(Arc::new(f)))
    }

    pub fn none() -> Self {
        Reporter(None)
    }

    pub fn report(&self, progress: Progress) {
        if let Some(f) = &self.0 {
            f(progress);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn reports_to_the_callback() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let reporter = Reporter::new(move |p| sink.lock().unwrap().push(p));
        reporter
            .clone()
            .report(Progress::Details { done: 1, total: 2 });
        Reporter::none().report(Progress::Details { done: 9, total: 9 });
        assert_eq!(
            *seen.lock().unwrap(),
            [Progress::Details { done: 1, total: 2 }]
        );
    }

    #[test]
    fn elapsed_formatting() {
        assert_eq!(elapsed(0), "0s");
        assert_eq!(elapsed(59), "59s");
        assert_eq!(elapsed(65), "1m 05s");
        assert_eq!(elapsed(-3), "0s");
    }

    #[test]
    fn descriptions() {
        let searching = Progress::Searching {
            done: 2,
            total: 4,
            found: 35,
        };
        assert_eq!(searching.describe(), "searching 2/4 (35 PRs so far)");
        let retrying = Progress::Retrying {
            attempt: 2,
            max: 3,
            reason: "connection dropped".into(),
        };
        assert_eq!(retrying.describe(), "connection dropped; retrying (2/3)");
    }
}
