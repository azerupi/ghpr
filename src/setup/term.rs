//! Running the wizard full screen, fetching what it asks for in the background.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use super::preview::Sample;
use super::teams;
use super::wizard::{Cmd, Data, Outcome, SampleRequest, Wizard};
use super::write::Source;
use crate::github::Client;
use crate::profile::Profile;
use crate::progress::Reporter;
use crate::{auth, fetch};

/// PRs fetched per search for a view's sample.
const SAMPLE_LIMIT: u32 = 10;
/// Often enough to animate the spinners.
const TICK: Duration = Duration::from_millis(100);

type Clients = HashMap<Source, Result<Arc<Client>, String>>;

pub async fn run(mut wizard: Wizard) -> Result<Outcome> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(TICK);
    let mut clients = Clients::new();
    // Dropped (and so aborted) when setup ends: no fetches outlive it.
    let mut tasks = JoinSet::new();

    // Restores the terminal on panic too.
    let mut terminal = ratatui::init();
    let result = async {
        loop {
            if let Some(outcome) = wizard.outcome.take() {
                return Ok(outcome);
            }
            terminal.draw(|frame| super::ui::render(frame, &mut wizard))?;
            let cmds = tokio::select! {
                event = events.next() => match event {
                    Some(Ok(Event::Key(key))) => wizard.handle(key),
                    Some(Ok(_)) => continue, // resize etc.: just redraw
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(Outcome::Cancelled),
                },
                _ = ticker.tick() => {
                    wizard.tick(jiff::Timestamp::now());
                    continue;
                }
                Some(data) = rx.recv() => wizard.on_data(data),
            };
            for cmd in cmds {
                spawn(cmd, &mut clients, &mut tasks, &tx);
            }
        }
    }
    .await;
    ratatui::restore();
    result
}

fn spawn(
    cmd: Cmd,
    clients: &mut Clients,
    tasks: &mut JoinSet<()>,
    tx: &mpsc::UnboundedSender<Data>,
) {
    let source = match &cmd {
        Cmd::Teams(source) | Cmd::Count { source, .. } => source.clone(),
        Cmd::Sample(request) => request.source.clone(),
    };
    tracing::debug!(?cmd, "setup fetch");
    let client = clients
        .entry(source.clone())
        .or_insert_with(|| client(&source))
        .clone();
    let tx = tx.clone();
    tasks.spawn(async move {
        let data = match cmd {
            Cmd::Teams(source) => Data::Teams {
                result: match client {
                    Ok(client) => teams::fetch(&client).await.map_err(|e| format!("{e:#}")),
                    Err(e) => Err(e),
                },
                source,
            },
            Cmd::Count { source, team } => Data::Count {
                result: match client {
                    Ok(client) => client
                        .count(&teams::requests_query(&team))
                        .await
                        .map_err(|e| format!("{e:#}")),
                    Err(e) => Err(e),
                },
                source,
                team,
            },
            Cmd::Sample(request) => {
                let SampleRequest {
                    source,
                    view,
                    viewer,
                    weights,
                } = *request;
                let result = match client {
                    Ok(client) => {
                        fetch::fetch(&client, &view, viewer, SAMPLE_LIMIT, &Reporter::none())
                            .await
                            .map(|fetched| {
                                Box::new(Sample::new(
                                    view.clone(),
                                    fetched,
                                    &weights,
                                    jiff::Timestamp::now(),
                                ))
                            })
                            .map_err(|e| format!("{e:#}"))
                    }
                    Err(e) => Err(e),
                };
                Data::Sample {
                    source,
                    view,
                    result,
                }
            }
        };
        let _ = tx.send(data);
    });
}

/// A client with the token for `source`, made once per account.
fn client(source: &Source) -> Result<Arc<Client>, String> {
    let profile = Profile {
        name: source.suggested_name(),
        gh_user: source.gh_user().map(str::to_owned),
        token_env: source.token_env().map(str::to_owned),
        reason: "setup".to_owned(),
    };
    auth::token(Some(&profile))
        .and_then(Client::new)
        .map(Arc::new)
        .map_err(|e| format!("{e:#}"))
}
