//! The interactive table.

mod app;
mod facets;
pub(crate) mod input;
mod ui;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;

pub use app::{App, Settings, TabSettings};
use app::{Cmd, FetchedData, Msg};

use crate::github::Client;
use crate::model::Viewer;
use crate::progress::Reporter;
use crate::views::View;
use crate::{cache, clipboard, fetch, state};

/// Everything needed to (re)fetch views in the background and save local state.
pub struct Source {
    /// `None` when offline.
    pub client: Option<Arc<Client>>,
    /// One per tab, in the same order.
    pub tabs: Vec<TabSource>,
    pub viewer: Viewer,
    pub limit: u32,
    pub state_path: Option<PathBuf>,
}

pub struct TabSource {
    pub view: View,
    pub cache_path: Option<PathBuf>,
}

pub async fn run(mut app: App, source: Source) -> Result<()> {
    let source = Arc::new(source);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = EventStream::new();
    // Fast enough to animate the refresh spinner; cheap otherwise.
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    let mut terminal = ratatui::init();
    let result = async {
        let cmds = app.init();
        run_cmds(cmds, &source, &tx);
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
                Msg::Tick(_) | Msg::Flash(_) => {}
            }
            let cmds = app.update(msg);
            run_cmds(cmds, &source, &tx);
        }
        Ok(())
    }
    .await;
    ratatui::restore();
    result
}

fn run_cmds(cmds: Vec<Cmd>, source: &Arc<Source>, tx: &mpsc::UnboundedSender<Msg>) {
    for cmd in cmds {
        match &cmd {
            Cmd::Fetch(tab) => tracing::debug!(tab, "refresh"),
            Cmd::Open(url) => tracing::info!(%url, "open"),
            Cmd::Copy(text) => tracing::info!(lines = text.lines().count(), "copy"),
            Cmd::SaveState(_) => tracing::trace!("save state"),
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
    let fetched = fetch::fetch(
        client,
        &tab.view,
        source.viewer.clone(),
        source.limit,
        reporter,
    )
    .await
    .map_err(|e| format!("refresh failed: {e:#}"))?;
    let at = jiff::Timestamp::now();
    if let Some(path) = &tab.cache_path {
        // A cache write failure only costs a slower next start; not worth an error.
        let _ = cache::save(path, &fetched.prs, &fetched.viewer, at);
    }
    Ok(FetchedData {
        prs: fetched.prs,
        viewer: fetched.viewer,
        truncated: !fetched.truncated.is_empty(),
        at,
    })
}
