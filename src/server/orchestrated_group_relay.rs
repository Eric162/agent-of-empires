//! OrchestratedGroup relay: posts worker lifecycle edges to the board and delivers unread board
//! messages into members once they can take input, so no agent has to poll.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;

use super::push::StatusChange;
use super::AppState;
use crate::session::orchestrated_group::{self, Board, BoardMessage, MessageKind, SYSTEM_AUTHOR};
use crate::session::{Group, Instance, Status, Storage};

/// Also the debounce: an edge is acted on at the next tick only if the status still holds.
const TICK: Duration = Duration::from_secs(3);
const SEND_TIMEOUT: Duration = Duration::from_secs(60);

struct Delivery {
    profile: String,
    session_id: String,
    topic: String,
    through_seq: u64,
    text: String,
}

pub fn spawn(state: Arc<AppState>) {
    crate::task_util::spawn_supervised(
        "server.orchestrated_group_relay",
        crate::task_util::PanicPolicy::Log,
        run(state),
    );
}

async fn run(state: Arc<AppState>) {
    let mut rx = state.status_tx.subscribe();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut edges: Vec<StatusChange> = Vec::new();
    loop {
        tokio::select! {
            recv = rx.recv() => {
                match recv {
                    Ok(change) if is_worker_edge(&change) => edges.push(change),
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
                continue;
            }
            _ = tick.tick() => {}
            _ = state.shutdown.cancelled() => return,
        }
        let instances = state.instances.read().await.clone();
        let edges = std::mem::take(&mut edges);
        let deliveries = match tokio::task::spawn_blocking(move || plan(&instances, &edges)).await {
            Ok(Ok(d)) => d,
            Ok(Err(error)) => {
                tracing::warn!(%error, "orchestrated group relay pass failed");
                continue;
            }
            Err(_) => continue,
        };
        for d in deliveries {
            deliver(d).await;
        }
    }
}

fn is_worker_edge(c: &StatusChange) -> bool {
    c.old == Status::Running && matches!(c.new, Status::Idle | Status::Waiting | Status::Error)
}

/// Terminal panes only take input safely when idle; ACP sessions queue or steer.
fn can_receive(inst: &Instance) -> bool {
    if inst.is_structured() {
        matches!(
            inst.status,
            Status::Idle | Status::Running | Status::Waiting
        )
    } else {
        inst.status == Status::Idle
    }
}

fn plan(instances: &[Instance], edges: &[StatusChange]) -> Result<Vec<Delivery>> {
    let mut by_profile: HashMap<&str, Vec<Instance>> = HashMap::new();
    for inst in instances {
        by_profile
            .entry(inst.source_profile.as_str())
            .or_default()
            .push(inst.clone());
    }
    let mut groups: Vec<(String, Vec<Group>, Vec<Instance>)> = Vec::new();
    for (profile, members) in by_profile {
        let (_, profile_groups) = Storage::open_unwatched(profile)?.load_with_groups()?;
        if profile_groups.iter().any(|g| g.orchestration.is_some()) {
            groups.push((profile.to_string(), profile_groups, members));
        }
    }
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    let mut board = Board::open()?;

    for edge in edges {
        for (profile, profile_groups, members) in &groups {
            let Some(inst) = members.iter().find(|i| i.id == edge.instance_id) else {
                continue;
            };
            if inst.status != edge.new {
                continue;
            }
            let Some(group) = orchestrated_group::find_for_path(profile_groups, &inst.group_path)
            else {
                continue;
            };
            let info = group
                .orchestration
                .as_ref()
                .expect("filtered on orchestration");
            if info.orchestrator_session_id.as_deref() == Some(inst.id.as_str()) {
                continue;
            }
            let hint = match inst.status {
                Status::Idle => "finished its turn; check the board for its result",
                Status::Waiting => "is waiting for input (permission or question in its pane)",
                _ => "hit an error",
            };
            board.post(
                &info.id,
                &BoardMessage {
                    kind: MessageKind::System,
                    from: SYSTEM_AUTHOR.into(),
                    from_title: SYSTEM_AUTHOR.into(),
                    to: None,
                    body: format!("worker '{}' ({}) {hint}", inst.title, inst.id),
                    at: Utc::now(),
                    channel: None,
                },
            )?;
            if inst.status == Status::Idle {
                release_floor_after_turn(profile, &group.path, info, inst)?;
            }
        }
    }

    let mut out = Vec::new();
    for (profile, profile_groups, members) in &groups {
        for group in profile_groups.iter().filter(|g| g.orchestration.is_some()) {
            let info = group
                .orchestration
                .as_ref()
                .expect("filtered on orchestration");
            if info.orchestrator_session_id.is_none() {
                continue;
            }
            for member in orchestrated_group::members(group, members) {
                let (msgs, through_seq) = board.pending_for(&info.id, &member.id, info)?;
                if msgs.is_empty() {
                    board.advance_cursor(&info.id, &member.id, through_seq)?;
                    continue;
                }
                if !can_receive(member) {
                    continue;
                }
                let text = orchestrated_group::delivery_text(&msgs);
                out.push(Delivery {
                    profile: profile.clone(),
                    session_id: member.id.clone(),
                    topic: info.id.clone(),
                    through_seq,
                    text,
                });
            }
        }
    }
    Ok(out)
}

/// A floor holder that ends its turn releases the floor so another member can take it.
/// Silent on purpose: announcing it would start the other agent's turn, and a pair
/// would then alternate forever.
fn release_floor_after_turn(
    profile: &str,
    group_path: &str,
    info: &orchestrated_group::OrchestrationInfo,
    inst: &Instance,
) -> Result<()> {
    let held: Vec<String> = info
        .channels_of(&inst.id)
        .filter(|c| c.floor.as_deref() == Some(inst.id.as_str()))
        .map(|c| c.name.clone())
        .collect();
    if held.is_empty() {
        return Ok(());
    }
    Storage::open_unwatched(profile)?.update(|_, groups| {
        if let Some(info) = groups
            .iter_mut()
            .find(|g| g.path == group_path)
            .and_then(|g| g.orchestration.as_mut())
        {
            for name in &held {
                if let Some(c) = info.open_channel_mut(name) {
                    c.floor = None;
                }
            }
        }
        Ok(())
    })
}

/// Reuses `aoe send` so terminal and structured sessions take the same path as the CLI.
async fn deliver(d: Delivery) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            tracing::warn!(%error, "orchestrated group relay: no current exe");
            return;
        }
    };
    let mut cmd = tokio::process::Command::new(exe);
    if !d.profile.is_empty() {
        cmd.args(["-p", &d.profile]);
    }
    cmd.args(["send", "--no-revive", &d.session_id, &d.text])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(SEND_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            tracing::warn!(%error, session = %d.session_id, "orchestrated group relay: send failed to start");
            return;
        }
        Err(_) => {
            tracing::warn!(session = %d.session_id, "orchestrated group relay: send timed out");
            return;
        }
    };
    if !output.status.success() {
        tracing::warn!(
            session = %d.session_id,
            stderr = %String::from_utf8_lossy(&output.stderr),
            "orchestrated group relay: send failed"
        );
        return;
    }
    let advanced = tokio::task::spawn_blocking(move || {
        Board::open()?.advance_cursor(&d.topic, &d.session_id, d.through_seq)
    })
    .await;
    if !matches!(advanced, Ok(Ok(()))) {
        tracing::warn!("orchestrated group relay: delivered but could not advance cursor");
    }
}
