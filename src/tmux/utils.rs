//! tmux utility functions

use anyhow::{bail, Result};
use std::sync::OnceLock;

pub fn strip_ansi(content: &str) -> String {
    let mut result = strip_osc_st(content);

    while let Some(start) = result.find("\x1b[") {
        let rest = &result[start + 2..];
        let end_offset = rest
            .find(|c: char| c.is_ascii_alphabetic())
            .map(|i| i + 1)
            .unwrap_or(rest.len());
        result = format!("{}{}", &result[..start], &result[start + 2 + end_offset..]);
    }

    while let Some(start) = result.find("\x1b]") {
        if let Some(end) = result[start..].find('\x07') {
            result = format!("{}{}", &result[..start], &result[start + end + 1..]);
        } else {
            break;
        }
    }

    result
}

/// Only targets ST-terminated (`\x1b\\`) OSC sequences; BEL-terminated ones
/// must pass through unchanged since downstream parsers handle those correctly.
pub(crate) fn strip_osc_st(content: &str) -> String {
    const OSC: &str = "\x1b]";
    const ST: &str = "\x1b\\";

    let mut result = String::with_capacity(content.len());
    let mut remaining = content;

    while let Some(osc_start) = remaining.find(OSC) {
        result.push_str(&remaining[..osc_start]);
        let payload = &remaining[osc_start + OSC.len()..];

        let bel_pos = payload.find('\x07');
        let st_pos = payload.find(ST);

        match (bel_pos, st_pos) {
            (Some(b), Some(s)) if b < s => {
                let end = osc_start + OSC.len() + b + 1;
                result.push_str(&remaining[osc_start..end]);
                remaining = &remaining[end..];
            }
            (_, Some(s)) => {
                remaining = &payload[s + ST.len()..];
            }
            _ => {
                result.push_str(&remaining[osc_start..osc_start + OSC.len()]);
                remaining = &remaining[osc_start + OSC.len()..];
            }
        }
    }
    result.push_str(remaining);
    result
}

pub fn sanitize_session_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(20)
        .collect()
}

/// Append `; set-option -p -t <target> remain-on-exit on` to an in-flight
/// tmux argument list so that remain-on-exit is set atomically with session
/// creation. Using pane-level (`-p`) avoids bleeding into user-created panes
/// in the same session.
///
/// Note: the `-p` (pane-level) flag requires tmux >= 3.0.
pub fn append_remain_on_exit_args(args: &mut Vec<String>, target: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        target.to_string(),
        "remain-on-exit".to_string(),
        "on".to_string(),
    ]);
}

/// Append `; set-option -t <target> pane-base-index 0` to an in-flight tmux
/// argument list so that pane indices always start at 0 regardless of the
/// user's global config.  This lets status checks use `.0` to reliably target
/// the agent's pane.  See #488.
pub fn append_pane_base_index_args(args: &mut Vec<String>, target: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-t".to_string(),
        target.to_string(),
        "pane-base-index".to_string(),
        "0".to_string(),
    ]);
}

/// Append `; set-option -t <target> default-shell <shell>` so panes the user
/// later splits off this session use their real shell instead of the shared
/// tmux server's frozen `default-shell` (which a dev build with a sandboxed
/// env can poison; see #2608). The first pane is launched with an explicit
/// login-shell command at create time because a `default-shell` set chained
/// after `new-session` is too late for the already-spawned pane.
pub fn append_default_shell_args(args: &mut Vec<String>, target: &str, shell: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-t".to_string(),
        target.to_string(),
        "default-shell".to_string(),
        shell.to_string(),
    ]);
}

/// Append `; set-option -t <target> mouse on` to an in-flight tmux argument
/// list so that mouse/wheel events are forwarded into tmux copy-mode.
///
/// Required for the web dashboard's two-finger scroll on mobile when the
/// underlying agent uses tmux copy-mode for scrollback (the default
/// renderer for Claude Code, and all other agents). Claude Code's
/// fullscreen renderer (`/tui fullscreen`) bypasses tmux copy-mode: it
/// runs on the alternate screen and relies on alternate-scroll turning the
/// wheel into arrow keys (it binds the arrows to scroll), so this option is
/// harmless but unused in that mode.
pub fn append_mouse_on_args(args: &mut Vec<String>, target: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-t".to_string(),
        target.to_string(),
        "mouse".to_string(),
        "on".to_string(),
    ]);
}

/// Append `; set-option -t <target> window-size latest` so the tmux window
/// follows the most recently active client. Required for the primary-client
/// resize model: without this, a user's `~/.tmux.conf` could set
/// `window-size smallest`, which would shrink the window to the smallest
/// attached PTY regardless of which client is primary.
pub fn append_window_size_args(args: &mut Vec<String>, target: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-t".to_string(),
        target.to_string(),
        "window-size".to_string(),
        "latest".to_string(),
    ]);
}

/// Append the two tmux options required for OSC 52 clipboard escapes from
/// the wrapped agent (Claude Code, OpenCode, Codex, etc.) to reach the outer
/// terminal. Without these, "select to copy" inside the agent silently fails
/// because tmux drops the sequence (see #897).
///
/// Two distinct mechanisms are covered:
///   * `set-clipboard on` (server option): captures and forwards raw OSC 52
///     sequences to attached terminal clients.
///   * `allow-passthrough on` (window option, added in tmux 3.3): allows
///     `\ePtmux;...\e\\`-wrapped escapes (the form OpenCode uses) to be
///     unwrapped and forwarded.
///
/// Programs vary in which form they emit, so both are set defensively. Scope
/// flags are explicit (`-s`, `-w`) so the call site is unambiguous and
/// resilient to future tmux scope-inference changes; matches the convention
/// used by `append_remain_on_exit_args` for `remain-on-exit`.
///
/// `-q` (silently ignore errors) keeps aoe compatible with tmux < 3.3, where
/// `allow-passthrough` does not exist. On those versions the set-option call
/// quietly no-ops instead of failing the whole `new-session` invocation.
pub fn append_clipboard_passthrough_args(args: &mut Vec<String>, target: &str) {
    args.extend([
        ";".to_string(),
        "set-option".to_string(),
        "-q".to_string(),
        "-s".to_string(),
        "set-clipboard".to_string(),
        "on".to_string(),
        ";".to_string(),
        "set-option".to_string(),
        "-q".to_string(),
        "-w".to_string(),
        "-t".to_string(),
        target.to_string(),
        "allow-passthrough".to_string(),
        "on".to_string(),
    ]);
}

/// Pin the first window's name to `window_name` so the tab reads e.g.
/// `agent (claude)` or `terminal` instead of tmux's default.
///
/// tmux's default `automatic-rename on` names a window after
/// `#{pane_current_command}`, which for an agent installed at a versioned path
/// (Claude Code lives at `.../claude/versions/2.1.220/claude`) renders as a bare
/// version string. Both rename mechanisms are disabled explicitly:
///   * `automatic-rename off` stops the command-name tracking.
///   * `allow-rename off` stops the agent's own OSC 2 title escapes from
///     overriding the name we just set.
///
/// Best-effort: a missing session or a tmux ENOENT is swallowed, since a tab
/// label is cosmetic and must never fail a launch. Chained into one invocation
/// so the window is never briefly visible under the wrong name.
pub fn set_window_name(session_name: &str, window_name: &str) {
    let target = format!("{session_name}:^");
    let _ = crate::tmux::tmux_command()
        .args([
            "set-option",
            "-w",
            "-t",
            &target,
            "automatic-rename",
            "off",
            ";",
            "set-option",
            "-w",
            "-t",
            &target,
            "allow-rename",
            "off",
            ";",
            "rename-window",
            "-t",
            &target,
            window_name,
        ])
        .output();
}

/// Make `source`'s first window appear as an additional tab in `dest`, without
/// moving it: tmux linked windows belong to both sessions at once, so
/// `source:^.0` keeps resolving to the same pane and every existing target
/// string in aoe stays valid.
///
/// `-a` appends after the highest index rather than colliding with an existing
/// one. Returns false when the link could not be made (either session missing,
/// tmux unavailable), so the caller can fall back to the separate-session flow.
///
/// Already-linked windows are detected up front and reported as success: tmux is
/// happy to link one window into the same session twice, at two indices, which
/// would show the user duplicate tabs onto the same pane.
pub fn link_window_into(source: &str, dest: &str) -> bool {
    let Some(window_id) = first_window_id(source) else {
        tracing::debug!(target: "tmux.window",
            source = %source, dest = %dest, "link-window skipped: source window not found");
        return false;
    };
    if window_index_in(dest, &window_id).is_some() {
        return true;
    }

    let src_target = format!("{source}:^");
    let dest_target = format!("{dest}:");
    match crate::tmux::tmux_command()
        .args(["link-window", "-a", "-s", &src_target, "-t", &dest_target])
        .output()
    {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            tracing::debug!(target: "tmux.window",
                source = %source, dest = %dest,
                "link-window failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            false
        }
        Err(e) => {
            tracing::debug!(target: "tmux.window",
                source = %source, dest = %dest, "link-window spawn failed: {}", e);
            false
        }
    }
}

/// Make `session_name`'s own first window current, undoing a previous
/// [`select_linked_window`].
///
/// A tmux session remembers its current window, and `attach-session` resumes
/// there. Selecting a paired terminal's tab therefore persists: without this, a
/// later agent attach would land on the terminal tab instead of the agent.
///
/// `^` is the lowest-numbered window, which is the surface's own: the session is
/// created with it, and `link_window_into` appends paired terminals after it.
/// Best-effort; a missing session is not an error.
pub fn select_first_window(session_name: &str) {
    let _ = crate::tmux::tmux_command()
        .args(["select-window", "-t", &format!("{session_name}:^")])
        .output();
}

/// Destroy `session_name`'s first window, removing it from *every* session that
/// holds it.
///
/// This is the teardown counterpart of [`link_window_into`] and the reason a
/// paired terminal cannot be torn down with `kill-session` alone: a linked window
/// survives its originating session's death for as long as another session holds
/// it, so killing the terminal's session leaves a dead-pane tab stranded in the
/// agent's session. Killing the window removes the tab everywhere, and tmux then
/// destroys any session left with no windows.
///
/// Best-effort; a missing session or window is not an error.
pub fn kill_first_window(session_name: &str) {
    let _ = crate::tmux::tmux_command()
        .args(["kill-window", "-t", &format!("{session_name}:^")])
        .output();
}

/// Make the window `source` owns current in `host`, so attaching `host` lands
/// on that tab. Returns false when the window is not linked into `host` (or
/// tmux is unreachable), letting the caller fall back to attaching `source`
/// directly.
///
/// The window is addressed by its tmux window id (`@N`, stable across
/// index renumbering) but selected by its index *within `host`*: a linked window
/// belongs to several sessions, so `select-window -t @N` alone leaves it
/// ambiguous which session's current window moves.
pub fn select_linked_window(host: &str, source: &str) -> bool {
    let Some(window_id) = first_window_id(source) else {
        return false;
    };
    let Some(index) = window_index_in(host, &window_id) else {
        return false;
    };
    crate::tmux::tmux_command()
        .args(["select-window", "-t", &format!("{host}:{index}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// tmux window id (`@N`) of `session_name`'s first window.
fn first_window_id(session_name: &str) -> Option<String> {
    crate::tmux::tmux_command()
        .args([
            "display-message",
            "-t",
            &format!("{session_name}:^"),
            "-p",
            "#{window_id}",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Index of the window with `window_id` as seen from `host`, or `None` when that
/// window is not one of `host`'s.
fn window_index_in(host: &str, window_id: &str) -> Option<String> {
    let output = crate::tmux::tmux_command()
        .args([
            "list-windows",
            "-t",
            host,
            "-F",
            "#{window_index} #{window_id}",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    stdout.lines().find_map(|line| {
        let (index, id) = line.trim().split_once(' ')?;
        (id == window_id).then(|| index.to_string())
    })
}

pub fn is_pane_dead(session_name: &str) -> bool {
    // Use `^.0` to target the first window's first pane regardless of
    // base-index or which pane is active, so the check always hits the
    // agent's pane even when the user has created additional tmux windows
    // or split panes.  See #435, #488.
    let target = format!("{session_name}:^.0");
    crate::tmux::tmux_command()
        .args(["display-message", "-t", &target, "-p", "#{pane_dead}"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

pub(crate) fn pane_current_command(session_name: &str) -> Option<String> {
    // Use `^.0` to target the first window's first pane regardless of
    // base-index or which pane is active.  See #435, #488.
    let target = format!("{session_name}:^.0");
    crate::tmux::tmux_command()
        .args([
            "display-message",
            "-t",
            &target,
            "-p",
            "#{pane_current_command}",
        ])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// Shells that indicate the agent is not running (the pane was restored by
// tmux-resurrect, the agent crashed back to a prompt, or the user exited).
const KNOWN_SHELLS: &[&str] = &[
    "bash", "zsh", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "pwsh",
];

pub(crate) fn is_shell_command(cmd: &str) -> bool {
    let normalized = cmd.strip_prefix('-').unwrap_or(cmd);
    KNOWN_SHELLS.contains(&normalized)
}

pub fn is_pane_running_shell(session_name: &str) -> bool {
    pane_current_command(session_name)
        .map(|cmd| is_shell_command(&cmd))
        .unwrap_or(false)
}

/// Returns the tmux prefix key formatted for display (e.g. "Ctrl+a", "Ctrl+b").
/// Reads `tmux show-option -gv prefix` once on first call and caches the
/// result; falls back to "Ctrl+b" if tmux is unavailable or the option can't
/// be parsed. The prefix can't change while AOE is running, so caching avoids
/// per-render-frame subprocess calls from the welcome dialog.
pub fn tmux_prefix_display() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        let raw = crate::tmux::tmux_command()
            .args(["show-option", "-gv", "prefix"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        format_tmux_prefix(&raw)
    })
}

/// Run `tmux kill-session -t <name>`. A missing session is treated as
/// success, since the goal is "this session is not present": `can't find
/// session` (the session is gone, e.g. callers commonly kill the pane's
/// process tree first, which can tear the session down before this lands)
/// and `no server running` (no tmux server at all, so no session exists)
/// are both swallowed in the C locale. Any other tmux failure returns
/// `Err`. Caller is responsible for `refresh_session_cache` after a
/// successful kill.
pub(crate) fn kill_session_if_present(name: &str) -> Result<()> {
    let output = crate::tmux::tmux_command()
        .env("LC_ALL", "C")
        .args(["kill-session", "-t", name])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let absent = stderr.contains("can't find session")
            || stderr.contains("no server running")
            || stderr.contains("error connecting");
        if !absent {
            bail!("Failed to kill tmux session '{}': {}", name, stderr);
        }
    }
    Ok(())
}

/// Convert tmux's raw prefix notation (e.g. "C-a", "M-b", "F12") to the
/// display form shown in UI hints. Preserves case from tmux so users see the
/// same letter they typed in `~/.tmux.conf`.
fn format_tmux_prefix(raw: &str) -> String {
    if let Some(key) = raw.strip_prefix("C-") {
        format!("Ctrl+{key}")
    } else if let Some(key) = raw.strip_prefix("M-") {
        format!("Alt+{key}")
    } else if !raw.is_empty() {
        raw.to_string()
    } else {
        "Ctrl+b".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::test_helpers::TmuxTestSession;

    #[test]
    fn test_sanitize_session_name() {
        assert_eq!(sanitize_session_name("my-project"), "my-project");
        assert_eq!(sanitize_session_name("my project"), "my_project");
        assert_eq!(sanitize_session_name("a".repeat(30).as_str()).len(), 20);
    }

    fn tmux_available_for_window_tests() -> bool {
        crate::tmux::tmux_command()
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn spawn_probe_session(name: &str) {
        let output = crate::tmux::tmux_command()
            .args([
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "80",
                "-y",
                "24",
                "sleep 30",
            ])
            .output()
            .expect("tmux new-session");
        assert!(output.status.success(), "failed to create {name}");
    }

    fn window_names_in(session: &str) -> Vec<String> {
        let out = crate::tmux::tmux_command()
            .args(["list-windows", "-t", session, "-F", "#{window_name}"])
            .output()
            .expect("tmux list-windows");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .collect()
    }

    /// `set_window_name` must survive tmux's own renaming: with the default
    /// `automatic-rename on` the window would be renamed back to
    /// `#{pane_current_command}` (`sleep` here, a bare version string for a
    /// real agent), so this locks the `automatic-rename off` half of the fix.
    #[test]
    #[serial_test::serial]
    fn set_window_name_pins_the_tab_label() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let guard = TmuxTestSession::new("aoe_test_winname");
        spawn_probe_session(guard.name());

        set_window_name(guard.name(), "agent (claude)");

        assert_eq!(window_names_in(guard.name()), vec!["agent (claude)"]);

        let automatic = crate::tmux::tmux_command()
            .args([
                "show-options",
                "-w",
                "-v",
                "-t",
                &format!("{}:^", guard.name()),
                "automatic-rename",
            ])
            .output()
            .expect("tmux show-options");
        assert_eq!(
            String::from_utf8_lossy(&automatic.stdout).trim(),
            "off",
            "automatic-rename must be off or tmux renames the tab back"
        );
    }

    /// The core of the shared-session model: after `link_window_into` the
    /// terminal's window is a second tab of the agent session, while the
    /// terminal session still exists and still resolves its own `:^.0` pane
    /// target. That last assertion is what keeps the change additive.
    #[test]
    #[serial_test::serial]
    fn link_window_into_adds_a_tab_without_moving_the_window() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_link_agent");
        let term = TmuxTestSession::new("aoe_test_link_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(agent.name(), "agent (claude)");
        set_window_name(term.name(), "terminal");

        assert!(link_window_into(term.name(), agent.name()));

        assert_eq!(
            window_names_in(agent.name()),
            vec!["agent (claude)", "terminal"],
            "the agent session should now show both tabs, agent first"
        );
        assert_eq!(
            window_names_in(term.name()),
            vec!["terminal"],
            "the terminal session must still own its window"
        );

        let pane = crate::tmux::tmux_command()
            .args([
                "display-message",
                "-t",
                &format!("{}:^.0", term.name()),
                "-p",
                "#{pane_dead}",
            ])
            .output()
            .expect("tmux display-message");
        assert!(
            pane.status.success(),
            "`<terminal>:^.0` must keep resolving after linking, or every \
             existing pane target in aoe breaks"
        );
    }

    /// `kill-session` is not enough to tear down a linked terminal: tmux keeps the
    /// window alive for as long as another session holds it, stranding a dead-pane
    /// tab in the agent session that the next terminal start then doubles up on.
    /// This pins the semantics that force `kill_first_window` to exist.
    #[test]
    #[serial_test::serial]
    fn kill_session_alone_strands_a_linked_window() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_strand_agent");
        let term = TmuxTestSession::new("aoe_test_strand_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(term.name(), "terminal");
        assert!(link_window_into(term.name(), agent.name()));

        kill_session_if_present(term.name()).expect("kill terminal session");

        assert!(
            window_names_in(agent.name()).contains(&"terminal".to_string()),
            "documents the tmux behavior the teardown fix works around"
        );
    }

    /// The bug-2 fix: killing the window drops the tab from the agent session too,
    /// so exiting and restarting a terminal cannot accumulate stale tabs.
    #[test]
    #[serial_test::serial]
    fn kill_first_window_removes_the_tab_from_the_host() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_killwin_agent");
        let term = TmuxTestSession::new("aoe_test_killwin_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(agent.name(), "agent (claude)");
        set_window_name(term.name(), "terminal");
        assert!(link_window_into(term.name(), agent.name()));

        kill_first_window(term.name());

        assert_eq!(
            window_names_in(agent.name()),
            vec!["agent (claude)"],
            "the terminal tab must be gone from the agent session"
        );
        assert!(
            !crate::tmux::session_exists(term.name()),
            "tmux should destroy the terminal session once its last window dies"
        );
    }

    /// Re-linking an already-linked window is treated as success, so a repeated
    /// terminal start (or a respawn after a dead pane) is idempotent rather than
    /// logging a spurious failure.
    #[test]
    #[serial_test::serial]
    fn link_window_into_is_idempotent() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_relink_agent");
        let term = TmuxTestSession::new("aoe_test_relink_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());

        assert!(link_window_into(term.name(), agent.name()));
        assert!(link_window_into(term.name(), agent.name()));
        assert_eq!(
            window_names_in(agent.name()).len(),
            2,
            "re-linking must not append a duplicate tab"
        );
    }

    /// The bug-1 shape: a restart kills and recreates the agent session, which
    /// drops its window list while the terminal's own session survives. Re-linking
    /// must restore the tab, with the terminal's pane (and scrollback) intact,
    /// rather than the terminal appearing destroyed.
    #[test]
    #[serial_test::serial]
    fn relinking_restores_the_tab_after_the_agent_session_is_recreated() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_restart_agent");
        let term = TmuxTestSession::new("aoe_test_restart_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(term.name(), "terminal");
        assert!(link_window_into(term.name(), agent.name()));
        let pane_pid_before = crate::process::get_pane_pid(term.name());
        assert!(pane_pid_before.is_some());

        // What `Instance::kill_clean` does on restart: kill only the agent session.
        kill_session_if_present(agent.name()).expect("kill agent session");
        assert!(
            crate::tmux::session_exists(term.name()),
            "the terminal session must outlive an agent restart"
        );
        spawn_probe_session(agent.name());
        set_window_name(agent.name(), "agent (claude)");
        assert_eq!(
            window_names_in(agent.name()),
            vec!["agent (claude)"],
            "the recreated agent session starts with no terminal tabs"
        );

        assert!(link_window_into(term.name(), agent.name()));

        assert_eq!(
            window_names_in(agent.name()),
            vec!["agent (claude)", "terminal"],
            "the terminal tab should be back, after the agent tab"
        );
        assert_eq!(
            crate::process::get_pane_pid(term.name()),
            pane_pid_before,
            "re-linking must reuse the surviving pane, not respawn it"
        );
    }

    #[test]
    #[serial_test::serial]
    fn link_window_into_fails_for_missing_session() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let term = TmuxTestSession::new("aoe_test_link_orphan");
        spawn_probe_session(term.name());
        assert!(
            !link_window_into(term.name(), "aoe_test_link_no_such_session"),
            "linking into a missing session must report failure so the caller \
             falls back to the standalone attach"
        );
    }

    /// `select_linked_window` makes the linked tab current in the *host*, which
    /// is what lets the attach land on the terminal tab.
    #[test]
    #[serial_test::serial]
    fn select_linked_window_makes_the_tab_current_in_the_host() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_sel_agent");
        let term = TmuxTestSession::new("aoe_test_sel_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(agent.name(), "agent (claude)");
        set_window_name(term.name(), "terminal");
        assert!(link_window_into(term.name(), agent.name()));

        assert!(select_linked_window(agent.name(), term.name()));

        let current = crate::tmux::tmux_command()
            .args([
                "display-message",
                "-t",
                agent.name(),
                "-p",
                "#{window_name}",
            ])
            .output()
            .expect("tmux display-message");
        assert_eq!(
            String::from_utf8_lossy(&current.stdout).trim(),
            "terminal",
            "the host session's current window should be the linked terminal"
        );
    }

    /// tmux persists a session's current window, and `attach-session` resumes
    /// there, so selecting a terminal tab leaks into the *next* agent attach:
    /// pressing Enter on the agent would land on the terminal. `select_first_window`
    /// is what brings focus back to the agent's own tab.
    #[test]
    #[serial_test::serial]
    fn selecting_a_terminal_tab_persists_until_the_first_window_is_reselected() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_focus_agent");
        let term = TmuxTestSession::new("aoe_test_focus_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());
        set_window_name(agent.name(), "agent (claude)");
        set_window_name(term.name(), "terminal");
        assert!(link_window_into(term.name(), agent.name()));

        let current = |session: &str| -> String {
            let out = crate::tmux::tmux_command()
                .args(["display-message", "-t", session, "-p", "#{window_name}"])
                .output()
                .expect("tmux display-message");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        assert!(select_linked_window(agent.name(), term.name()));
        assert_eq!(
            current(agent.name()),
            "terminal",
            "documents the sticky current-window behavior behind the bug"
        );

        select_first_window(agent.name());

        assert_eq!(
            current(agent.name()),
            "agent (claude)",
            "an agent attach must resume on the agent tab, not the terminal"
        );
    }

    /// An unlinked terminal must report false rather than silently selecting
    /// something else, so `attach_via_host` falls back to its own session.
    #[test]
    #[serial_test::serial]
    fn select_linked_window_false_when_not_linked() {
        if !tmux_available_for_window_tests() {
            eprintln!("Skipping test: tmux not available");
            return;
        }
        let agent = TmuxTestSession::new("aoe_test_nolink_agent");
        let term = TmuxTestSession::new("aoe_test_nolink_term");
        spawn_probe_session(agent.name());
        spawn_probe_session(term.name());

        assert!(!select_linked_window(agent.name(), term.name()));
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("\x1b[32mgreen\x1b[0m"), "green");
        assert_eq!(strip_ansi("no codes here"), "no codes here");
        assert_eq!(strip_ansi("\x1b[1;34mbold blue\x1b[0m"), "bold blue");
    }

    #[test]
    fn test_strip_ansi_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_multiple_codes() {
        assert_eq!(
            strip_ansi("\x1b[1m\x1b[32mbold green\x1b[0m normal"),
            "bold green normal"
        );
    }

    #[test]
    fn test_strip_ansi_osc_bel() {
        assert_eq!(strip_ansi("\x1b]0;Window Title\x07text"), "text");
    }

    #[test]
    fn test_strip_ansi_osc_st() {
        assert_eq!(strip_ansi("\x1b]0;Window Title\x1b\\text"), "text");
    }

    #[test]
    fn test_strip_osc_st_hyperlink() {
        assert_eq!(
            strip_osc_st("\x1b]8;;https://example.com\x1b\\Click Here\x1b]8;;\x1b\\"),
            "Click Here"
        );
    }

    #[test]
    fn test_strip_osc_st_preserves_surrounding_text() {
        assert_eq!(
            strip_osc_st("before \x1b]8;;https://github.com\x1b\\link text\x1b]8;;\x1b\\ after"),
            "before link text after"
        );
    }

    #[test]
    fn test_strip_osc_st_multiple_links() {
        let input = "\x1b]8;;https://a.com\x1b\\A\x1b]8;;\x1b\\ and \x1b]8;;https://b.com\x1b\\B\x1b]8;;\x1b\\";
        assert_eq!(strip_osc_st(input), "A and B");
    }

    #[test]
    fn test_strip_osc_st_no_osc() {
        assert_eq!(strip_osc_st("plain text"), "plain text");
    }

    #[test]
    fn test_strip_osc_st_preserves_sgr() {
        assert_eq!(
            strip_osc_st("\x1b[32m\x1b]8;;url\x1b\\green link\x1b]8;;\x1b\\\x1b[0m"),
            "\x1b[32mgreen link\x1b[0m"
        );
    }

    #[test]
    fn test_strip_osc_st_unterminated() {
        assert_eq!(
            strip_osc_st("\x1b]8;;url without terminator"),
            "\x1b]8;;url without terminator"
        );
    }

    #[test]
    fn test_strip_osc_st_passes_bel_terminated_through() {
        let bel_osc = "\x1b]0;Window Title\x07";
        assert_eq!(strip_osc_st(bel_osc), bel_osc);
    }

    #[test]
    fn test_strip_osc_st_mixed_bel_then_st() {
        let input = "\x1b]0;Title\x07before\x1b]8;;https://x.com\x1b\\link\x1b]8;;\x1b\\after";
        assert_eq!(strip_osc_st(input), "\x1b]0;Title\x07beforelinkafter");
    }

    #[test]
    fn test_strip_ansi_nested_sequences() {
        assert_eq!(strip_ansi("\x1b[38;5;196mred\x1b[0m"), "red");
    }

    #[test]
    fn test_strip_ansi_with_256_colors() {
        assert_eq!(
            strip_ansi("\x1b[38;2;255;100;50mRGB color\x1b[0m"),
            "RGB color"
        );
    }

    #[test]
    fn test_sanitize_session_name_special_chars() {
        assert_eq!(sanitize_session_name("test/path"), "test_path");
        assert_eq!(sanitize_session_name("test.name"), "test_name");
        assert_eq!(sanitize_session_name("test@name"), "test_name");
        assert_eq!(sanitize_session_name("test:name"), "test_name");
    }

    #[test]
    fn test_sanitize_session_name_preserves_valid_chars() {
        assert_eq!(sanitize_session_name("test-name_123"), "test-name_123");
    }

    #[test]
    fn test_sanitize_session_name_empty() {
        assert_eq!(sanitize_session_name(""), "");
    }

    #[test]
    fn test_sanitize_session_name_unicode() {
        let result = sanitize_session_name("test😀emoji");
        assert!(result.starts_with("test"));
        assert!(result.contains('_'));
        assert!(!result.contains('😀'));
    }

    #[test]
    fn test_is_shell_command_recognizes_common_shells() {
        for shell in KNOWN_SHELLS {
            assert!(
                is_shell_command(shell),
                "{shell} should be recognized as a shell"
            );
        }
    }

    #[test]
    fn test_is_shell_command_recognizes_login_shells() {
        for shell in ["-bash", "-zsh", "-sh", "-fish"] {
            assert!(
                is_shell_command(shell),
                "{shell} should be recognized as a login shell"
            );
        }
    }

    #[test]
    fn test_is_shell_command_rejects_agent_binaries() {
        for cmd in [
            "claude", "opencode", "codex", "gemini", "cursor", "droid", "sleep", "python",
        ] {
            assert!(
                !is_shell_command(cmd),
                "{cmd} should not be recognized as a shell"
            );
        }
    }

    #[test]
    fn test_format_tmux_prefix_ctrl() {
        assert_eq!(format_tmux_prefix("C-a"), "Ctrl+a");
        assert_eq!(format_tmux_prefix("C-b"), "Ctrl+b");
        assert_eq!(format_tmux_prefix("C-Space"), "Ctrl+Space");
    }

    #[test]
    fn test_format_tmux_prefix_alt() {
        assert_eq!(format_tmux_prefix("M-x"), "Alt+x");
    }

    #[test]
    fn test_format_tmux_prefix_preserves_case() {
        // tmux returns the prefix in whatever case the user wrote it; preserve
        // it so the displayed hint matches their muscle memory.
        assert_eq!(format_tmux_prefix("C-A"), "Ctrl+A");
        assert_eq!(format_tmux_prefix("C-b"), "Ctrl+b");
    }

    #[test]
    fn test_format_tmux_prefix_special_keys() {
        assert_eq!(format_tmux_prefix("F12"), "F12");
        assert_eq!(format_tmux_prefix("Space"), "Space");
    }

    #[test]
    fn test_format_tmux_prefix_empty_falls_back() {
        assert_eq!(format_tmux_prefix(""), "Ctrl+b");
    }

    #[test]
    fn test_append_clipboard_passthrough_args() {
        let mut args: Vec<String> = vec!["new-session".into()];
        append_clipboard_passthrough_args(&mut args, "aoe_test");
        assert_eq!(
            args,
            vec![
                "new-session",
                ";",
                "set-option",
                "-q",
                "-s",
                "set-clipboard",
                "on",
                ";",
                "set-option",
                "-q",
                "-w",
                "-t",
                "aoe_test",
                "allow-passthrough",
                "on",
            ]
        );
    }

    #[test]
    fn test_append_default_shell_args() {
        let mut args: Vec<String> = vec!["new-session".into()];
        append_default_shell_args(&mut args, "aoe_test", "/bin/zsh");
        assert_eq!(
            args,
            vec![
                "new-session",
                ";",
                "set-option",
                "-t",
                "aoe_test",
                "default-shell",
                "/bin/zsh",
            ]
        );
    }

    fn tmux_available() -> bool {
        crate::tmux::tmux_command()
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    // Serialized like every test that talks to the shared tmux server: a
    // non-serial test that kills the server's last session makes the server
    // exit, and a `#[serial]` peer whose `new-session` connects inside that
    // teardown window fails with "server exited unexpectedly" (CI flake on
    // update_status_reconciles_running_hook_to_waiting_on_claude_approval_prompt).
    #[test]
    #[serial_test::serial]
    fn kill_session_if_present_swallows_missing_session() {
        if !tmux_available() {
            return;
        }
        let name = "aoe_test_kill_if_present_missing";
        let _ = crate::tmux::tmux_command()
            .args(["kill-session", "-t", name])
            .output();
        assert!(kill_session_if_present(name).is_ok());
    }

    #[test]
    #[serial_test::serial]
    fn kill_session_if_present_kills_existing_session() {
        if !tmux_available() {
            return;
        }
        let name = "aoe_test_kill_if_present_alive";
        let _ = crate::tmux::tmux_command()
            .args(["kill-session", "-t", name])
            .output();
        let spawn = crate::tmux::tmux_command()
            .args(["new-session", "-d", "-s", name])
            .status();
        if !spawn.map(|s| s.success()).unwrap_or(false) {
            return;
        }
        assert!(kill_session_if_present(name).is_ok());
        let exists = crate::tmux::tmux_command()
            .args(["has-session", "-t", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            !exists,
            "session should be gone after kill_session_if_present"
        );
    }
}
