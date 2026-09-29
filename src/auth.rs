use std::process::Command;

use color_eyre::eyre::{Result, bail};

use crate::profile::Profile;

/// Find a GitHub token. A profile's `token_env` or `gh_user` decides; without
/// one: `GH_TOKEN`, then `GITHUB_TOKEN`, then the gh CLI's active account.
pub fn token(profile: Option<&Profile>) -> Result<String> {
    if let Some(profile) = profile {
        if let Some(var) = &profile.token_env {
            return match std::env::var(var) {
                Ok(token) if !token.trim().is_empty() => {
                    tracing::info!(profile = %profile.name, source = %var, "using token from environment");
                    Ok(token.trim().to_owned())
                }
                _ => bail!(
                    "profile {:?} takes its token from ${var}, which isn't set",
                    profile.name
                ),
            };
        }
        if let Some(user) = &profile.gh_user {
            return match gh_token(Some(user)) {
                Some(token) => {
                    tracing::info!(profile = %profile.name, gh_user = %user, "using gh token");
                    Ok(token)
                }
                None => bail!(
                    "gh has no token for account {user:?} (profile {:?}); run `gh auth login` and log in as {user}",
                    profile.name
                ),
            };
        }
    }

    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(token) = std::env::var(var)
            && !token.trim().is_empty()
        {
            tracing::info!(source = var, "using token from environment");
            return Ok(token.trim().to_owned());
        }
    }
    if let Some(token) = gh_token(None) {
        tracing::info!("using gh's active account");
        return Ok(token);
    }
    bail!("no GitHub token found: run `gh auth login`, or set GH_TOKEN")
}

/// `gh auth token`, for a specific account or the active one.
fn gh_token(user: Option<&str>) -> Option<String> {
    let mut command = Command::new("gh");
    command.args(["auth", "token", "--hostname", "github.com"]);
    if let Some(user) = user {
        command.args(["--user", user]);
    }
    let output = command.output().ok()?;
    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !token.is_empty()).then_some(token)
}
