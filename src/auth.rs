use std::process::Command;

use color_eyre::eyre::{Result, bail};

/// Find a GitHub token: `GH_TOKEN`, then `GITHUB_TOKEN`, then the gh CLI's login.
pub fn token() -> Result<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(token) = std::env::var(var)
            && !token.trim().is_empty()
        {
            return Ok(token.trim().to_owned());
        }
    }

    let output = Command::new("gh").args(["auth", "token"]).output();
    if let Ok(output) = output
        && output.status.success()
    {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    bail!("no GitHub token found: run `gh auth login`, or set GH_TOKEN")
}
