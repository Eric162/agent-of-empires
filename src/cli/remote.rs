//! `aoe remote` subcommands: manage the remote `aoe serve` daemons whose
//! sessions are merged into the local session list.
//!
//! Config-only. Nothing here contacts a remote: `add` validates the URL's
//! shape and writes the entry, and the TUI's refresh loop reports
//! reachability on the row itself. That keeps `add` fast and usable while
//! the other box is still booting, and puts connection errors in the one
//! place the user is going to look for them.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde::Serialize;

use crate::acp::client::split_remote_url;
use crate::session::config::{load_config, update_config, RemoteConfig};

#[derive(Subcommand)]
pub enum RemoteCommands {
    /// List configured remotes
    #[command(alias = "ls")]
    List(RemoteListArgs),

    /// Add a remote daemon whose sessions appear in the local list
    Add(RemoteAddArgs),

    /// Remove a remote
    #[command(alias = "rm")]
    Remove(RemoteRemoveArgs),

    /// Enable a remote without re-entering its URL
    Enable(RemoteToggleArgs),

    /// Skip a remote's sessions without deleting its entry
    Disable(RemoteToggleArgs),
}

#[derive(Args)]
pub struct RemoteListArgs {
    /// Output as JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
pub struct RemoteAddArgs {
    /// Short label for the remote; badged onto each of its rows
    name: String,

    /// Base URL of the remote daemon, e.g. `http://linuxbox:8080`. A
    /// `?token=` copied from the remote's `aoe url` output is accepted and
    /// lifted into the stored token.
    url: String,

    /// Bearer token for the remote daemon. Omit for a `--no-auth` daemon or
    /// when the token is already in the URL.
    #[arg(long)]
    token: Option<String>,

    /// Replace an existing entry with the same name
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
pub struct RemoteRemoveArgs {
    /// Name of the remote to remove
    name: String,
}

#[derive(Args)]
pub struct RemoteToggleArgs {
    /// Name of the remote
    name: String,
}

#[derive(Serialize)]
struct RemoteListEntry {
    name: String,
    url: String,
    enabled: bool,
    /// Whether a token is stored, never the token itself: `aoe remote list`
    /// output ends up in issue reports and terminal scrollback.
    has_token: bool,
}

pub async fn run(command: RemoteCommands) -> Result<()> {
    match command {
        RemoteCommands::List(args) => list(args),
        RemoteCommands::Add(args) => add(args),
        RemoteCommands::Remove(args) => remove(args),
        RemoteCommands::Enable(args) => set_enabled(args, true),
        RemoteCommands::Disable(args) => set_enabled(args, false),
    }
}

fn list(args: RemoteListArgs) -> Result<()> {
    let config = load_config()?.unwrap_or_default();
    let entries: Vec<RemoteListEntry> = config
        .remotes
        .iter()
        .map(|(name, cfg)| RemoteListEntry {
            name: name.clone(),
            url: cfg.url.clone(),
            enabled: cfg.enabled,
            has_token: cfg.token.as_ref().is_some_and(|t| !t.is_empty()),
        })
        .collect();

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("No remotes configured. Add one with `aoe remote add <name> <url>`.");
        return Ok(());
    }

    for entry in entries {
        let state = if entry.enabled { "" } else { " (disabled)" };
        let auth = if entry.has_token { "" } else { " [no token]" };
        println!("{}\t{}{}{}", entry.name, entry.url, auth, state);
    }
    Ok(())
}

fn add(args: RemoteAddArgs) -> Result<()> {
    let name = args.name.trim().to_string();
    validate_name(&name)?;

    let (url, url_token) = split_remote_url(&args.url);
    validate_url(&url)?;
    // An explicit `--token` wins over one embedded in the URL, so a user
    // correcting a stale pasted URL does not silently keep the old token.
    let token = args
        .token
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(url_token);

    let existed = update_config(|config| {
        let existed = config.remotes.contains_key(&name);
        if existed && !args.force {
            return existed;
        }
        config.remotes.insert(
            name.clone(),
            RemoteConfig {
                url: url.clone(),
                token: token.clone(),
                enabled: true,
            },
        );
        existed
    })?;

    if existed && !args.force {
        bail!("remote '{name}' already exists; pass --force to replace it");
    }

    if existed {
        println!("Replaced remote '{}' -> {}", name, url);
    } else {
        println!("Added remote '{}' -> {}", name, url);
    }
    if token.is_none() {
        println!(
            "No token stored. If that daemon requires auth, re-add with --token \
             (find it in `aoe url` on the remote)."
        );
    }
    Ok(())
}

fn remove(args: RemoteRemoveArgs) -> Result<()> {
    let name = args.name.trim().to_string();
    let removed = update_config(|config| config.remotes.remove(&name).is_some())?;
    if !removed {
        bail!("no remote named '{name}'");
    }
    println!("Removed remote '{}'", name);
    Ok(())
}

fn set_enabled(args: RemoteToggleArgs, enabled: bool) -> Result<()> {
    let name = args.name.trim().to_string();
    let found = update_config(|config| match config.remotes.get_mut(&name) {
        Some(entry) => {
            entry.enabled = enabled;
            true
        }
        None => false,
    })?;
    if !found {
        bail!("no remote named '{name}'");
    }
    println!(
        "{} remote '{}'",
        if enabled { "Enabled" } else { "Disabled" },
        name
    );
    Ok(())
}

/// Names index a TOML table and are rendered as a section header, so keep
/// them to characters that need no quoting and cannot be confused with the
/// `<prefix>/<name>` section-path format.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("remote name cannot be empty");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("remote name may only contain letters, digits, '-', and '_'");
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("remote URL must start with http:// or https:// (got '{url}')");
    }
    // `http://` alone parses as a scheme with no authority; require a host.
    let host = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    if host.is_empty() || host.starts_with('/') {
        bail!("remote URL is missing a host (got '{url}')");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_plain_labels() {
        assert!(validate_name("linuxbox").is_ok());
        assert!(validate_name("build-box_2").is_ok());
    }

    #[test]
    fn validate_name_rejects_separators_and_empty() {
        // A '/' would collide with the `<prefix>/<name>` section-path format.
        assert!(validate_name("linux/box").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("has space").is_err());
    }

    #[test]
    fn validate_url_requires_scheme_and_host() {
        assert!(validate_url("http://linuxbox:8080").is_ok());
        assert!(validate_url("https://box.tailnet.ts.net").is_ok());
        assert!(validate_url("linuxbox:8080").is_err());
        assert!(validate_url("http://").is_err());
        assert!(validate_url("http:///nohost").is_err());
    }
}
