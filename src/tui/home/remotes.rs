//! Remote `aoe serve` daemons merged into the local session list.
//!
//! Each `[remotes.<name>]` config entry becomes a [`RemoteSource`]: a
//! synthetic bottom-shelf section, sibling of Archived and Trash, holding
//! the structured-view sessions that machine owns. Rows are read-only
//! mirrors of the remote daemon's `/api/sessions`; the only interaction
//! they support is opening the embedded structured view against the
//! remote endpoint, which is already endpoint-parameterized
//! (`EmbeddedView::connect`).
//!
//! Why remote rows are NOT `Instance`s in `HomeView::instances`: every
//! local operation (stop, delete, worktree edit, diff, tmux attach)
//! resolves its target through `instances`, against this machine's
//! filesystem and tmux server. A synthesized `Instance` would make all of
//! those silently addressable and wrong. Keeping remote rows in their own
//! collection behind a distinct [`crate::session::Item`] variant means an
//! operation that has not opted in cannot reach them at all.

use std::sync::mpsc;
use std::time::Duration;

use serde::Deserialize;

use crate::acp::client::{endpoint_for_remote, DaemonEndpoint, HttpClient};
use crate::session::config::RemoteConfig;
use crate::session::{Status, View};

/// How often each enabled remote is re-listed. Slower than the local
/// status poller's hot tier: this is a cross-network request per remote,
/// and a merged row's value is "what is running over there", which does
/// not need sub-second fidelity. The embedded structured view drives its
/// own WebSocket once opened, so an open session stays live regardless.
pub(crate) const REMOTE_REFRESH_INTERVAL: Duration = Duration::from_secs(6);

/// Subset of the daemon's `/api/sessions` row the merged list needs.
///
/// `serde` skips unknown fields, so a newer daemon adding columns does not
/// break an older client. Every field carries a `default` for the same
/// reason in reverse: an older daemon omitting one must not fail the whole
/// list and blank the section.
#[derive(Debug, Clone, Deserialize)]
pub struct RemoteSession {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project_path: String,
    /// The daemon serializes this with `format!("{:?}")`, so it arrives
    /// capitalized ("Running") rather than in `Status`'s lowercase serde
    /// repr. Parsed by [`parse_api_status`] instead of deserialized
    /// directly, so an unrecognized value degrades to `Unknown` rather
    /// than failing the row.
    #[serde(default)]
    pub status: String,
    /// How the remote session renders. Defaults to `terminal` so an older
    /// daemon's response (which omits the field) still deserializes.
    #[serde(default)]
    pub view: View,
}

impl RemoteSession {
    pub fn status(&self) -> Status {
        parse_api_status(&self.status)
    }
}

/// Map the daemon's `Debug`-formatted status onto [`Status`], tolerating
/// case differences and unknown variants. A remote running a newer `aoe`
/// may report a status this binary has never heard of; that must render as
/// Unknown, not drop the row.
pub fn parse_api_status(raw: &str) -> Status {
    match raw.trim().to_ascii_lowercase().as_str() {
        "running" => Status::Running,
        "waiting" => Status::Waiting,
        "idle" => Status::Idle,
        "stopped" => Status::Stopped,
        "error" => Status::Error,
        "starting" => Status::Starting,
        "deleting" => Status::Deleting,
        "creating" => Status::Creating,
        _ => Status::Unknown,
    }
}

/// One configured remote plus the last snapshot fetched from it.
pub(crate) struct RemoteSource {
    /// Config key; the label badged onto each row and used as the section
    /// header name.
    pub name: String,
    pub endpoint: DaemonEndpoint,
    /// Structured-view sessions from the last successful refresh. Retained
    /// across a failed refresh so a transient network blip dims the
    /// section rather than emptying it.
    pub sessions: Vec<RemoteSession>,
    /// Error text from the most recent refresh, cleared on success.
    pub last_error: Option<String>,
    /// True once a refresh has completed (either way), so the section can
    /// distinguish "connecting…" from "no sessions".
    pub loaded: bool,
    pub collapsed: bool,
    /// A fetch for this remote is outstanding. The client timeout (15s) is
    /// longer than the refresh interval (6s), so without this a remote that
    /// black-holes packets would accumulate two or three overlapping
    /// requests. Skipping it keeps at most one in flight per remote and
    /// makes the effective cadence honest rather than compounding.
    pub in_flight: bool,
}

impl RemoteSource {
    fn new(name: String, config: &RemoteConfig) -> Self {
        Self {
            name,
            endpoint: endpoint_for_remote(config),
            sessions: Vec::new(),
            last_error: None,
            loaded: false,
            collapsed: false,
            in_flight: false,
        }
    }

    /// Status line shown on the section header, right of the count.
    pub fn subtitle(&self) -> Option<String> {
        if let Some(err) = &self.last_error {
            return Some(format!("unreachable: {}", first_line(err)));
        }
        if !self.loaded {
            return Some("connecting…".to_string());
        }
        None
    }

    pub fn session(&self, id: &str) -> Option<&RemoteSession> {
        self.sessions.iter().find(|s| s.id == id)
    }
}

/// Keep only the first line of a client error. `HttpError`'s Display can
/// carry a multi-line reqwest chain, which would break the single-row
/// section header.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).trim().to_string()
}

/// A completed refresh for one remote, delivered back to the UI tick.
pub(crate) struct RemoteUpdate {
    pub name: String,
    pub result: Result<Vec<RemoteSession>, String>,
}

/// Build the source list from config, dropping disabled entries.
///
/// Sorted by name so the shelf order is stable across refreshes (config is
/// a `BTreeMap`, but being explicit keeps the guarantee local).
pub(crate) fn sources_from_config(
    remotes: &std::collections::BTreeMap<String, RemoteConfig>,
) -> Vec<RemoteSource> {
    let mut sources: Vec<RemoteSource> = remotes
        .iter()
        .filter(|(_, cfg)| cfg.enabled && !cfg.url.trim().is_empty())
        .map(|(name, cfg)| RemoteSource::new(name.clone(), cfg))
        .collect();
    sources.sort_by(|a, b| a.name.cmp(&b.name));
    sources
}

/// Spawn one fetch per remote that does not already have one outstanding,
/// reporting each back over `tx`.
///
/// Fire-and-forget: a task whose send fails (UI torn down) just ends. The
/// caller re-arms on its own cadence rather than looping in here, so a hung
/// request cannot pile up behind an interval timer.
pub(crate) fn spawn_refresh(sources: &mut [RemoteSource], tx: &mpsc::Sender<RemoteUpdate>) {
    for source in sources {
        if source.in_flight {
            continue;
        }
        source.in_flight = true;
        let name = source.name.clone();
        let endpoint = source.endpoint.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = fetch_sessions(endpoint).await;
            let _ = tx.send(RemoteUpdate { name, result });
        });
    }
}

/// List a remote's structured-view sessions.
///
/// Terminal-view sessions are filtered out here rather than at render
/// time: a remote tmux pane cannot be attached from this machine, so a row
/// for one would advertise an action the TUI has no way to perform.
async fn fetch_sessions(endpoint: DaemonEndpoint) -> Result<Vec<RemoteSession>, String> {
    let client = HttpClient::new(endpoint).map_err(|e| e.to_string())?;
    let sessions = client
        .list_sessions::<RemoteSession>()
        .await
        .map_err(|e| e.to_string())?;
    let mut structured: Vec<RemoteSession> = sessions
        .into_iter()
        .filter(|s| s.view == View::Structured)
        .collect();
    structured.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(structured)
}

/// Test-only source pre-loaded with a session list, so view tests can
/// exercise remote rows without standing up a daemon.
#[cfg(test)]
pub(crate) fn test_source(name: &str, sessions: Vec<RemoteSession>) -> RemoteSource {
    let mut source = RemoteSource::new(
        name.to_string(),
        &RemoteConfig {
            url: "http://remote.test:8080".to_string(),
            token: None,
            enabled: true,
        },
    );
    source.sessions = sessions;
    source.loaded = true;
    source
}

/// Test-only structured remote session row.
#[cfg(test)]
pub(crate) fn test_session(id: &str, title: &str, status: &str) -> RemoteSession {
    RemoteSession {
        id: id.to_string(),
        title: title.to_string(),
        project_path: "/remote/repo".to_string(),
        status: status.to_string(),
        view: View::Structured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_status_accepts_debug_casing() {
        // The daemon emits `format!("{:?}", status)`, not the lowercase
        // serde repr, so the capitalized forms are the real wire values.
        assert_eq!(parse_api_status("Running"), Status::Running);
        assert_eq!(parse_api_status("Waiting"), Status::Waiting);
        assert_eq!(parse_api_status("idle"), Status::Idle);
    }

    #[test]
    fn parse_api_status_unknown_variant_degrades() {
        // A newer daemon reporting a status this binary lacks must not
        // drop the row.
        assert_eq!(parse_api_status("Hibernating"), Status::Unknown);
        assert_eq!(parse_api_status(""), Status::Unknown);
    }

    #[test]
    fn sources_from_config_skips_disabled_and_blank() {
        let mut remotes = std::collections::BTreeMap::new();
        remotes.insert(
            "on".to_string(),
            RemoteConfig {
                url: "http://a:8080".into(),
                token: None,
                enabled: true,
            },
        );
        remotes.insert(
            "off".to_string(),
            RemoteConfig {
                url: "http://b:8080".into(),
                token: None,
                enabled: false,
            },
        );
        remotes.insert(
            "blank".to_string(),
            RemoteConfig {
                url: "   ".into(),
                token: None,
                enabled: true,
            },
        );
        let sources = sources_from_config(&remotes);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "on");
    }

    #[test]
    fn subtitle_reports_connecting_then_error() {
        let cfg = RemoteConfig {
            url: "http://a:8080".into(),
            token: None,
            enabled: true,
        };
        let mut source = RemoteSource::new("a".into(), &cfg);
        assert_eq!(source.subtitle().as_deref(), Some("connecting…"));
        source.loaded = true;
        assert_eq!(source.subtitle(), None);
        source.last_error = Some("connection refused\nsecond line".into());
        assert_eq!(
            source.subtitle().as_deref(),
            Some("unreachable: connection refused")
        );
    }
}
