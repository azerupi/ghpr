//! Logging to a file, for looking into a session after the fact.
//!
//! Logs go to `$XDG_STATE_HOME/ghpr/logs/ghpr.<date>.log` (one file per day,
//! a week kept). `GHPR_LOG` sets the level, e.g. `debug` or `ghpr=trace`.
//! Tokens are never logged.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "ghpr=info";
const KEEP_FILES: usize = 7;

pub fn dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "ghpr")?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
    Some(base.join("logs"))
}

/// Start logging. Keep the guard alive until exit so buffered lines get
/// written. Logging problems never stop ghpr; it just runs without a log.
pub fn init() -> Option<WorkerGuard> {
    let dir = dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("ghpr")
        .filename_suffix("log")
        .max_log_files(KEEP_FILES)
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter =
        EnvFilter::try_from_env("GHPR_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .try_init()
        .ok()?;

    // Record panics too; the terminal is usually gone by the time anyone looks.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!(%info, "panic");
        previous(info);
    }));
    Some(guard)
}
