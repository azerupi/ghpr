//! The interactive table.

mod app;
mod facets;
pub(crate) mod input;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

pub use app::{App, NotifySettings, Settings, TabSettings};
use app::{Cmd, FetchedData, Msg};

use crate::github::Client;
use crate::model::Viewer;
use crate::progress::Reporter;
use crate::views::View;
use crate::{cache, clipboard, fetch, notify, state};

/// Set once showing a notification failed: no point trying (and failing)
/// again on every refresh.
static NOTIFY_FAILED: AtomicBool = AtomicBool::new(false);

/// Everything needed to (re)fetch views in the background and save local state.
pub struct Source {
    /// `None` when offline.
    pub client: Option<Arc<Client>>,
    /// One per tab, in the same order.
    pub tabs: Vec<TabSource>,
    pub viewer: Viewer,
    pub limit: u32,
    pub state_path: Option<PathBuf>,
    /// Held by the one instance per profile that notifies.
    pub notify_lock: Option<PathBuf>,
}

pub struct TabSource {
    pub view: View,
    pub cache_path: Option<PathBuf>,
}

pub async fn run(mut app: App, source: Source) -> Result<()> {
    let source = Arc::new(source);
    let (tx, mut rx) = mpsc::unbounded_channel();
    // Kept open for the whole session; closing it (or exiting) releases it.
    let mut lock: Option<std::fs::File> = None;
    let mut events = EventStream::new();
    // Fast enough to animate the refresh spinner; cheap otherwise.
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    let mut terminal = ratatui::init();
    let result = async {
        let cmds = app.init();
        run_cmds(cmds, &source, &tx, &mut lock);
        while !app.quit {
            terminal.draw(|frame| ui::render(frame, &mut app))?;
            let msg = tokio::select! {
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) => Msg::Key(key),
                    Some(Ok(_)) => continue, // resize etc.: just redraw
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                _ = ticker.tick() => Msg::Tick(jiff::Timestamp::now()),
                Some(msg) = rx.recv() => msg,
            };
            match &msg {
                Msg::Key(key) => tracing::trace!(?key, "key"),
                Msg::Fetched {
                    tab,
                    result: Err(error),
                } => tracing::warn!(tab, %error, "refresh failed"),
                Msg::Fetched {
                    tab,
                    result: Ok(data),
                } => {
                    tracing::debug!(tab, prs = data.prs.len(), "refreshed");
                }
                Msg::Progress { tab, progress } => tracing::trace!(tab, ?progress, "progress"),
                Msg::PrState { id, result } => tracing::debug!(%id, ?result, "pr state"),
                Msg::NotifyLock(acquired) => tracing::debug!(acquired, "notify lock"),
                Msg::Tick(_) | Msg::Flash(_) => {}
            }
            let cmds = app.update(msg);
            run_cmds(cmds, &source, &tx, &mut lock);
        }
        Ok(())
    }
    .await;
    ratatui::restore();
    result
}

fn run_cmds(
    cmds: Vec<Cmd>,
    source: &Arc<Source>,
    tx: &mpsc::UnboundedSender<Msg>,
    lock: &mut Option<std::fs::File>,
) {
    for cmd in cmds {
        match &cmd {
            Cmd::Fetch(tab) => tracing::debug!(tab, "refresh"),
            Cmd::Open(url) => tracing::info!(%url, "open"),
            Cmd::Copy(text) => tracing::info!(lines = text.lines().count(), "copy"),
            Cmd::SaveState(_) => tracing::trace!("save state"),
            Cmd::Notify(notices) => {
                for n in notices {
                    tracing::info!(title = %n.title, "notify");
                }
            }
            Cmd::LookupPr(id) => tracing::debug!(%id, "look up pr"),
            Cmd::TryNotifyLock => tracing::trace!("try notify lock"),
        }
        match cmd {
            Cmd::Fetch(tab) => {
                let Some(client) = source.client.clone() else {
                    continue;
                };
                let source = Arc::clone(source);
                let tx = tx.clone();
                tokio::spawn(async move {
                    let progress_tx = tx.clone();
                    let reporter = Reporter::new(move |progress| {
                        let _ = progress_tx.send(Msg::Progress { tab, progress });
                    });
                    let result = fetch_view(&client, &source, tab, &reporter).await;
                    let _ = tx.send(Msg::Fetched { tab, result });
                });
            }
            Cmd::SaveState(local) => {
                if let Some(path) = &source.state_path
                    && let Err(e) = state::save(path, &local)
                {
                    let _ = tx.send(Msg::Flash(format!("couldn't save state: {e:#}")));
                }
            }
            Cmd::Open(url) => {
                let text = match open::that_detached(&url) {
                    Ok(()) => format!("opened {url}"),
                    Err(e) => format!("couldn't open browser: {e}"),
                };
                let _ = tx.send(Msg::Flash(text));
            }
            Cmd::Notify(notices) => {
                if NOTIFY_FAILED.load(Ordering::Relaxed) {
                    continue;
                }
                let tx = tx.clone();
                // The notifier is a separate program; don't hold up the UI.
                std::thread::spawn(move || {
                    for notice in &notices {
                        if let Err(e) = notify::send(notice) {
                            if !NOTIFY_FAILED.swap(true, Ordering::Relaxed) {
                                tracing::warn!(error = %e, "notifications off for this session");
                                let _ = tx
                                    .send(Msg::Flash(format!("couldn't show a notification: {e}")));
                            }
                            break;
                        }
                    }
                });
            }
            Cmd::TryNotifyLock => {
                let acquired = match &source.notify_lock {
                    // Nowhere to keep a lock: notify, as without one.
                    None => true,
                    Some(path) => match notify::try_lock(path) {
                        Ok(Some(file)) => {
                            tracing::info!(path = %path.display(), "notifying (holding the notify lock)");
                            *lock = Some(file);
                            true
                        }
                        Ok(None) => false,
                        Err(e) => {
                            tracing::warn!(path = %path.display(), error = %e, "can't lock; notifying anyway");
                            true
                        }
                    },
                };
                let _ = tx.send(Msg::NotifyLock(acquired));
            }
            Cmd::LookupPr(id) => {
                let Some(client) = source.client.clone() else {
                    continue;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = client.pr_state(&id).await.map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::PrState { id, result });
                });
            }
            Cmd::Copy(text) => {
                let flash = match clipboard::copy(&text) {
                    Ok(()) => format!("copied {text}"),
                    Err(e) => e.to_string(),
                };
                let _ = tx.send(Msg::Flash(flash));
            }
        }
    }
}

async fn fetch_view(
    client: &Client,
    source: &Source,
    tab: usize,
    reporter: &Reporter,
) -> Result<FetchedData, String> {
    let tab = &source.tabs[tab];
    let previous = cache::fallback(tab.cache_path.as_deref(), jiff::Timestamp::now());
    let fetched = fetch::fetch(
        client,
        &tab.view,
        source.viewer.clone(),
        previous.as_ref(),
        source.limit,
        reporter,
    )
    .await
    .map_err(|e| format!("refresh failed: {e:#}"))?;
    let at = fetched.stale_since.unwrap_or_else(jiff::Timestamp::now);
    // Stale results are already what's cached; keeping their time stops them
    // standing in for longer than the fallback allows.
    if let Some(path) = &tab.cache_path
        && fetched.stale_since.is_none()
    {
        // A cache write failure only costs a slower next start; not worth an error.
        let _ = cache::save(path, &fetched.prs, &fetched.viewer, at);
    }
    Ok(FetchedData {
        prs: fetched.prs,
        viewer: fetched.viewer,
        truncated: !fetched.truncated.is_empty(),
        stale: fetched.stale_since.is_some(),
        at,
    })
}
