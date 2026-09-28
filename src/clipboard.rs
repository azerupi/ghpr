//! Copying to the system clipboard.
//!
//! External tools are preferred because they keep serving the clipboard after
//! ghpr exits; on Linux an in-process clipboard would lose its contents.

use std::io::Write;
use std::process::{Command, Stdio};

use color_eyre::eyre::{Result, bail};

const TOOLS: &[(&str, &[&str])] = &[
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
    ("pbcopy", &[]),
    ("clip.exe", &[]),
];

pub fn copy(text: &str) -> Result<()> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    for (tool, args) in TOOLS {
        if *tool == "wl-copy" && !wayland {
            continue;
        }
        let Ok(mut child) = Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        if child.wait()?.success() {
            return Ok(());
        }
    }
    bail!("no clipboard tool found (install wl-clipboard or xclip)")
}
