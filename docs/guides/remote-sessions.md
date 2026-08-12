# Merged Remote Sessions

Show the sessions running on another machine in your local `aoe` TUI, alongside your own.

If you keep sessions on a laptop and also run agents on a Linux box, this puts both lists on one screen. Each remote gets its own section at the bottom of the sidebar, below Archived and Trash, and you can open a remote session's transcript and composer without leaving the TUI.

## What this is (and is not)

The merged list is a **read-only mirror plus a live transcript**. A remote row lets you read the session, watch it work, and send it messages through the structured view. It does not let you run local operations against it.

| | Local sessions | Remote sessions |
|---|---|---|
| Read transcript, send messages | yes | yes |
| Stop, delete, restart, rename | yes | no |
| Diff view, worktree edits | yes | no |
| tmux attach | yes | no |
| Create new sessions | yes | no |

Two limits are worth understanding before you set this up:

- **Structured-view sessions only.** A remote terminal (tmux) session is not listed, because its pane cannot be attached from another machine without SSH'ing into the host first. Showing the row would advertise something the TUI cannot do.
- **No local filesystem.** A remote session's worktree lives on the other machine, so the diff view, file watching, and worktree editing do not apply.

For anything outside that, use the remote's own web dashboard (`aoe serve`), or SSH in and run `aoe` there.

## Setup

### 1. Run a daemon on the remote machine

On the Linux box:

```bash
aoe serve --host 0.0.0.0
```

The daemon must be reachable from your local machine. On a LAN, `--host 0.0.0.0` plus the box's IP is enough. Across networks, put both machines on a tailnet and use the Tailscale hostname; see [Tailscale Setup](tailscale.md).

Print the URL and token:

```bash
aoe url
```

### 2. Add the remote locally

On your laptop:

```bash
aoe remote add linuxbox http://linuxbox:8080 --token <token>
```

You can also paste the tokenized URL from `aoe url` directly; the token is lifted out of the query string and stored in its own field, so it never sits in the URL on disk:

```bash
aoe remote add linuxbox 'http://linuxbox:8080/?token=abc123'
```

The name (`linuxbox`) is the label badged onto each row and used as the section header, so keep it short. Letters, digits, `-`, and `_` only.

### 3. Open the TUI

Remote sections appear at the bottom of the sidebar within a few seconds:

```
  my-local-session                  ⠿ running
  another-local                     ⠒ idle
▼ ⊘ Trash (1)
▼ ⇄ linuxbox (3)
    refactor-parser                 ⠿
    fix-flaky-test                  ⠛
    docs-pass                       ⠒
```

Press Enter on a remote row to open its structured view in the preview pane, the same embedded view local structured sessions use. Enter on the section header collapses it.

## Managing remotes

```bash
aoe remote list              # show configured remotes (never prints tokens)
aoe remote list --json
aoe remote add <name> <url>  # --token <t>, --force to replace
aoe remote remove <name>
aoe remote disable <name>    # skip it without deleting the entry
aoe remote enable <name>
```

`disable` is the one to reach for when a box is off for a while: a disabled remote is skipped entirely, so it stops painting an error on every refresh. The entry stays, so re-enabling needs no URL or token.

Entries live in your config under `[remotes.<name>]`:

```toml
[remotes.linuxbox]
url = "http://linuxbox:8080"
token = "abc123"
enabled = true
```

## Reading the section header

The header carries the remote's connection state, so an unreachable box is never confused with one that simply has no sessions:

- `⇄ linuxbox (3)` — connected, three structured sessions.
- `⇄ linuxbox (0) connecting…` — first fetch has not returned yet.
- `⇄ linuxbox (2) unreachable: <error>` — the last refresh failed. The two rows are the previous snapshot, kept so a brief network blip does not empty the section.

Remote rows use static status glyphs rather than the animated spinners local rows use. The list is re-fetched every few seconds, so an animation would imply a liveness this side does not actually sample. Once you open a remote session, its structured view holds its own WebSocket and updates live.

## Security notes

- The token in `[remotes.<name>]` grants full dashboard access to that daemon. Your config file holds it in plaintext, so treat it like any other credential in a dotfile.
- Do not expose `aoe serve --host 0.0.0.0` to the open internet with a guessable token. Use a tailnet, a VPN, or `aoe serve --remote` with a passphrase.
- `aoe remote list` deliberately reports only whether a token is stored, not its value, so its output is safe to paste into a bug report.

## Related

- [Web Dashboard](web-dashboard.md) — the full `aoe serve` reference.
- [Tailscale Setup](tailscale.md) — reaching another machine across networks.
- [Structured View](../structured-view.md) — the transcript and composer a remote row opens.
