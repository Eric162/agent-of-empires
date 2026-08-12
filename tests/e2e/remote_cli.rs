//! End-to-end coverage for `aoe remote`, the CLI half of merged remote
//! sessions.
//!
//! Drives the real `aoe` binary as a subprocess (`run_cli`, no tmux, no
//! network) against an isolated home, then asserts on the persisted
//! `config.toml` and on what `list` prints. Every remote here points at a
//! host that does not resolve, which is the point: these subcommands are
//! config-only, so they must succeed without the other box existing.
//!
//! The assertions worth an e2e rather than a unit test are the ones about
//! the real file and the real stdout: that the entry survives the
//! serde round-trip through `config.toml` at all (the field is a
//! `skip_serializing_if` map, so a broken annotation would silently drop
//! it), that a token pasted into the URL is lifted out of the stored URL,
//! and that no code path prints a stored token back to the terminal.

use serial_test::parallel;

use crate::harness::TuiTestHarness;

fn config_path(h: &TuiTestHarness) -> std::path::PathBuf {
    crate::harness::app_dir_in(h.home_path()).join("config.toml")
}

fn read_config(h: &TuiTestHarness) -> String {
    let path = config_path(h);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e))
}

/// `remote list --json`, parsed. Uses the JSON form so assertions key off
/// field names rather than the human table's spacing.
fn list_json(h: &TuiTestHarness) -> serde_json::Value {
    let out = h.run_cli(&["remote", "list", "--json"]);
    assert!(
        out.status.success(),
        "remote list failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("remote list --json emitted invalid JSON")
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
#[parallel]
fn remote_add_persists_and_lists() {
    let h = TuiTestHarness::new("aoe_e2e_remote_add");

    let add = h.run_cli(&["remote", "add", "linuxbox", "http://linuxbox.invalid:8080"]);
    assert!(
        add.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );

    // Adding without a token must say so: a silent success against an
    // authenticated daemon would only surface later as an unreachable row.
    let added = stdout_of(&add);
    assert!(added.contains("Added remote 'linuxbox'"), "got: {added}");
    assert!(added.contains("No token stored"), "got: {added}");

    // The entry really round-trips through config.toml on disk.
    let config = read_config(&h);
    assert!(
        config.contains("[remotes.linuxbox]"),
        "config.toml is missing the remotes table:\n{config}"
    );

    let entries = list_json(&h);
    let entries = entries.as_array().expect("list is an array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "linuxbox");
    assert_eq!(entries[0]["url"], "http://linuxbox.invalid:8080");
    assert_eq!(entries[0]["enabled"], true);
    assert_eq!(entries[0]["has_token"], false);
}

#[test]
#[parallel]
fn remote_list_never_prints_the_token() {
    // `aoe remote list` output lands in bug reports and scrollback, so the
    // token must not appear in either output form, and `has_token` must
    // still report that one is stored.
    let h = TuiTestHarness::new("aoe_e2e_remote_token");
    let secret = "tok-do-not-leak-abc123";

    let add = h.run_cli(&[
        "remote",
        "add",
        "box",
        "http://box.invalid:8080",
        "--token",
        secret,
    ]);
    assert!(add.status.success());
    assert!(
        !stdout_of(&add).contains(secret),
        "remote add echoed the token back"
    );

    let plain = h.run_cli(&["remote", "list"]);
    assert!(plain.status.success());
    assert!(
        !stdout_of(&plain).contains(secret),
        "remote list leaked the token: {}",
        stdout_of(&plain)
    );

    let json = list_json(&h);
    assert!(
        !json.to_string().contains(secret),
        "remote list --json leaked the token"
    );
    assert_eq!(json[0]["has_token"], true);

    // The token is stored, just never printed.
    assert!(
        read_config(&h).contains(secret),
        "the token was not persisted at all"
    );
}

#[test]
#[parallel]
fn remote_add_lifts_token_out_of_a_pasted_url() {
    // `aoe url` on the remote emits `...?token=<t>`; pasting that whole
    // string must not leave the credential sitting in the stored URL, which
    // is the field `list` prints.
    let h = TuiTestHarness::new("aoe_e2e_remote_pasted");
    let secret = "url-embedded-token-xyz";

    let add = h.run_cli(&[
        "remote",
        "add",
        "pasted",
        &format!("http://box.invalid:8080/?token={secret}"),
    ]);
    assert!(
        add.status.success(),
        "remote add failed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    // A token was found, so the no-token warning must not fire.
    assert!(!stdout_of(&add).contains("No token stored"));

    let json = list_json(&h);
    assert_eq!(json[0]["url"], "http://box.invalid:8080");
    assert_eq!(json[0]["has_token"], true);
    assert!(!json.to_string().contains(secret));
}

#[test]
#[parallel]
fn remote_add_refuses_duplicate_without_force() {
    let h = TuiTestHarness::new("aoe_e2e_remote_dup");
    assert!(h
        .run_cli(&["remote", "add", "dup", "http://a.invalid:8080"])
        .status
        .success());

    let again = h.run_cli(&["remote", "add", "dup", "http://b.invalid:8080"]);
    assert!(!again.status.success(), "duplicate add should fail");
    let err = String::from_utf8_lossy(&again.stderr);
    assert!(err.contains("--force"), "unhelpful error: {err}");

    // The original URL must survive a refused add.
    assert_eq!(list_json(&h)[0]["url"], "http://a.invalid:8080");

    let forced = h.run_cli(&["remote", "add", "dup", "http://b.invalid:8080", "--force"]);
    assert!(forced.status.success());
    assert!(stdout_of(&forced).contains("Replaced remote 'dup'"));
    assert_eq!(list_json(&h)[0]["url"], "http://b.invalid:8080");
}

#[test]
#[parallel]
fn remote_disable_enable_and_remove_round_trip() {
    let h = TuiTestHarness::new("aoe_e2e_remote_toggle");
    assert!(h
        .run_cli(&["remote", "add", "box", "http://box.invalid:8080"])
        .status
        .success());

    // Disable keeps the entry (and its URL) so re-enabling needs no retyping.
    assert!(h.run_cli(&["remote", "disable", "box"]).status.success());
    let json = list_json(&h);
    assert_eq!(json[0]["enabled"], false);
    assert_eq!(json[0]["url"], "http://box.invalid:8080");
    assert!(stdout_of(&h.run_cli(&["remote", "list"])).contains("(disabled)"));

    assert!(h.run_cli(&["remote", "enable", "box"]).status.success());
    assert_eq!(list_json(&h)[0]["enabled"], true);

    assert!(h.run_cli(&["remote", "remove", "box"]).status.success());
    assert!(list_json(&h).as_array().expect("array").is_empty());
    assert!(
        stdout_of(&h.run_cli(&["remote", "list"])).contains("No remotes configured"),
        "empty list should tell the user how to add one"
    );

    // Removing something that is gone is an error, not a silent no-op.
    let gone = h.run_cli(&["remote", "remove", "box"]);
    assert!(!gone.status.success());
    assert!(String::from_utf8_lossy(&gone.stderr).contains("no remote named 'box'"));
}

#[test]
#[parallel]
fn remote_add_rejects_bad_names_and_urls() {
    let h = TuiTestHarness::new("aoe_e2e_remote_validate");

    // A '/' would collide with the section-path format the TUI uses to
    // address a remote's synthetic group.
    let slash = h.run_cli(&["remote", "add", "linux/box", "http://a.invalid:8080"]);
    assert!(!slash.status.success());

    let no_scheme = h.run_cli(&["remote", "add", "box", "a.invalid:8080"]);
    assert!(!no_scheme.status.success());
    assert!(String::from_utf8_lossy(&no_scheme.stderr).contains("http://"));

    // Nothing was written by either failure.
    assert!(list_json(&h).as_array().expect("array").is_empty());
}
