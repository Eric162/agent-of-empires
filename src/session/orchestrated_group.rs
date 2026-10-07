//! OrchestratedGroups: a group run by an Orchestrator Agent session that delegates to worker
//! sessions and coordinates through a durable message board.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{Group, Instance};
use crate::events::{self, Order, Schema, SeqBound};

pub const SKILL_DIR: &str = "aoe-orchestrated-group";
pub const SKILL_MD: &str = include_str!("orchestrated_group_skill.md");

/// Board author for messages the relay writes.
pub const SYSTEM_AUTHOR: &str = "aoe";
/// Board author for messages posted from outside any session.
pub const USER_AUTHOR: &str = "user";
/// `to` value addressing every worker.
pub const ALL_WORKERS: &str = "all";

const SCHEMA_PREFIX: &str = "board";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrchestrationInfo {
    /// Stable board topic and memory directory name; survives group renames.
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestrator_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<String>,
    /// Topic name to the sessions subscribed to it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub topics: BTreeMap<String, Vec<String>>,
}

impl OrchestrationInfo {
    pub fn new(brief: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            orchestrator_session_id: None,
            brief,
            topics: BTreeMap::new(),
        }
    }

    pub fn subscribed(&self, topic: &str, session: &str) -> bool {
        self.topics
            .get(topic)
            .is_some_and(|subs| subs.iter().any(|s| s == session))
    }

    pub fn subscribe(&mut self, topic: &str, session: &str) {
        let subs = self.topics.entry(topic.to_string()).or_default();
        if !subs.iter().any(|s| s == session) {
            subs.push(session.to_string());
        }
    }

    pub fn unsubscribe(&mut self, topic: &str, session: &str) {
        if let Some(subs) = self.topics.get_mut(topic) {
            subs.retain(|s| s != session);
            if subs.is_empty() {
                self.topics.remove(topic);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum MessageKind {
    Task,
    Status,
    Question,
    Answer,
    Result,
    Learning,
    Note,
    System,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Status => "status",
            Self::Question => "question",
            Self::Answer => "answer",
            Self::Result => "result",
            Self::Learning => "learning",
            Self::Note => "note",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardMessage {
    pub kind: MessageKind,
    /// Session id, [`SYSTEM_AUTHOR`] or [`USER_AUTHOR`].
    pub from: String,
    pub from_title: String,
    /// Session id or [`ALL_WORKERS`]; `None` addresses the orchestrator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub body: String,
    pub at: DateTime<Utc>,
    /// Topic tag; topic posts reach the topic's subscribers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
}

impl BoardMessage {
    /// Whether `reader` should receive this message: anything addressed to it; a topic
    /// post's subscribers; otherwise the orchestrator for untargeted posts and workers
    /// for broadcasts.
    pub fn is_for(&self, reader: &str, info: &OrchestrationInfo) -> bool {
        if self.from == reader {
            return false;
        }
        let orchestrator = info.orchestrator_session_id.as_deref().unwrap_or_default();
        match (self.to.as_deref(), self.topic.as_deref()) {
            (Some(ALL_WORKERS), _) => reader != orchestrator,
            (Some(to), _) => to == reader,
            (None, Some(topic)) => info.subscribed(topic, reader),
            (None, None) => reader == orchestrator,
        }
    }

    pub fn render(&self, seq: u64) -> String {
        let to = self
            .to
            .as_deref()
            .map(|t| format!(" -> {t}"))
            .unwrap_or_default();
        let topic = self
            .topic
            .as_deref()
            .map(|c| format!(" #{c}"))
            .unwrap_or_default();
        format!(
            "#{seq}{topic} [{}] {} ({}){to}: {}",
            self.kind.as_str(),
            self.from_title,
            self.from,
            self.body
        )
    }
}

/// What the relay types into a session: one compact line per message, prefixed so the
/// agent can tell board mail from its user.
pub fn delivery_text(msgs: &[(u64, BoardMessage)]) -> String {
    let line = |seq: u64, m: &BoardMessage| {
        let kind = match m.kind {
            MessageKind::Note => String::new(),
            k => format!(" {}", k.as_str()),
        };
        let topic = m
            .topic
            .as_deref()
            .map(|c| format!(" in #{c}"))
            .unwrap_or_default();
        format!(
            "#{seq}{kind}{topic} from {}: {}",
            m.from_title,
            m.body.trim()
        )
    };
    match msgs {
        [(seq, m)] => format!("[board] {}", line(*seq, m)),
        _ => {
            let mut text = format!("[board] {} new messages", msgs.len());
            for (seq, m) in msgs {
                text.push('\n');
                text.push_str(&line(*seq, m));
            }
            text
        }
    }
}

pub fn root_dir() -> Result<PathBuf> {
    Ok(super::get_app_dir()?.join("orchestrated_groups"))
}

pub fn memory_dir(info: &OrchestrationInfo) -> Result<PathBuf> {
    Ok(root_dir()?.join(&info.id))
}

/// Create the memory directory with a seed `MEMORY.md` if it is missing.
pub fn ensure_memory(info: &OrchestrationInfo, group_path: &str) -> Result<PathBuf> {
    let dir = memory_dir(info)?;
    std::fs::create_dir_all(&dir)?;
    let index = dir.join("MEMORY.md");
    if !index.exists() {
        let brief = info.brief.as_deref().unwrap_or("(none yet)");
        std::fs::write(
            &index,
            format!(
                "# {group_path} memory\n\nCurated by the Orchestrator Agent. Keep this file a short index; \
                 put detail in sibling topic files.\n\n## Brief\n\n{brief}\n\n## Index\n"
            ),
        )?;
    }
    Ok(dir)
}

/// Install or refresh the managed skill so sessions pick it up at launch.
pub fn install_skill() -> Result<()> {
    let dir = super::skills_model::managed_skills_dir()?.join(SKILL_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("SKILL.md");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SKILL_MD) {
        std::fs::write(&path, SKILL_MD)?;
    }
    Ok(())
}

/// The nearest group at or above `group_path` that is an OrchestratedGroup.
pub fn find_for_path<'a>(groups: &'a [Group], group_path: &str) -> Option<&'a Group> {
    let mut path = group_path;
    loop {
        if let Some(g) = groups
            .iter()
            .find(|g| g.path == path && g.orchestration.is_some())
        {
            return Some(g);
        }
        path = path.rsplit_once('/')?.0;
    }
}

/// Sessions taking part in `group`'s board: its orchestrator plus everything under the group.
pub fn members<'a>(group: &Group, instances: &'a [Instance]) -> Vec<&'a Instance> {
    let prefix = format!("{}/", group.path);
    let orchestrator = group
        .orchestration
        .as_ref()
        .and_then(|p| p.orchestrator_session_id.as_deref());
    instances
        .iter()
        .filter(|i| {
            Some(i.id.as_str()) == orchestrator
                || i.group_path == group.path
                || i.group_path.starts_with(&prefix)
        })
        .collect()
}

pub struct Board {
    conn: Connection,
    schema: Schema,
}

impl Board {
    pub fn open() -> Result<Self> {
        Self::open_at(&root_dir()?.join("board.db"))
    }

    pub fn open_at(path: &Path) -> Result<Self> {
        let schema = Schema::new(SCHEMA_PREFIX)?;
        let conn = events::open(path, &schema)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS board_cursors (
                topic  TEXT    NOT NULL,
                reader TEXT    NOT NULL,
                seq    INTEGER NOT NULL,
                PRIMARY KEY (topic, reader)
            );",
        )?;
        Ok(Self { conn, schema })
    }

    /// Append atomically across processes and return the assigned seq.
    pub fn post(&mut self, topic: &str, msg: &BoardMessage) -> Result<u64> {
        let json = serde_json::to_string(msg)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let seq = events::highest_seq(&tx, &self.schema, topic) + 1;
        events::insert_event(&tx, &self.schema, topic, seq, &json, msg.at.timestamp())?;
        tx.commit()?;
        Ok(seq)
    }

    pub fn read(&self, topic: &str, after: u64, limit: Option<usize>) -> Vec<(u64, BoardMessage)> {
        events::scan(
            &self.conn,
            &self.schema,
            topic,
            SeqBound::After(after),
            Order::Asc,
            limit,
        )
        .into_iter()
        .filter_map(|(seq, json)| serde_json::from_str(&json).ok().map(|m| (seq, m)))
        .collect()
    }

    pub fn highest_seq(&self, topic: &str) -> u64 {
        events::highest_seq(&self.conn, &self.schema, topic)
    }

    pub fn cursor(&self, topic: &str, reader: &str) -> Result<u64> {
        let seq: Option<i64> = self
            .conn
            .query_row(
                "SELECT seq FROM board_cursors WHERE topic = ?1 AND reader = ?2",
                params![topic, reader],
                |r| r.get(0),
            )
            .optional()?;
        Ok(seq.unwrap_or(0).max(0) as u64)
    }

    /// Only moves forward, so a late writer cannot cause redelivery.
    pub fn advance_cursor(&self, topic: &str, reader: &str, seq: u64) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO board_cursors (topic, reader, seq) VALUES (?1, ?2, ?3)
                 ON CONFLICT(topic, reader) DO UPDATE SET seq = MAX(seq, excluded.seq)",
                params![topic, reader, seq as i64],
            )
            .context("advance board cursor")?;
        Ok(())
    }

    /// Undelivered messages for `reader`, and the seq to advance its cursor to.
    pub fn pending_for(
        &self,
        topic: &str,
        reader: &str,
        info: &OrchestrationInfo,
    ) -> Result<(Vec<(u64, BoardMessage)>, u64)> {
        let after = self.cursor(topic, reader)?;
        let all = self.read(topic, after, None);
        let high = all.last().map(|(s, _)| *s).unwrap_or(after);
        let mine = all
            .into_iter()
            .filter(|(_, m)| m.is_for(reader, info))
            .collect();
        Ok((mine, high))
    }
}

/// The running binary, so agents reach this build and app dir rather than whatever `aoe`
/// is on their PATH.
pub fn aoe_bin() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "aoe".to_string())
}

pub fn identity_footer(session_id: &str) -> String {
    let aoe = aoe_bin();
    // Skills reach agents only when `skills.auto_propagate` is on, so name the file too.
    let skill = super::skills_model::managed_skills_dir()
        .map(|d| d.join(SKILL_DIR).join("SKILL.md").display().to_string())
        .unwrap_or_default();
    format!(
        "If the `{SKILL_DIR}` skill is not loaded, read {skill}. {}",
        identity_line(&aoe, session_id)
    )
}

fn identity_line(aoe: &str, session_id: &str) -> String {
    format!(
        "Run every `aoe` command, including those in the skill, as `{aoe}`. Your aoe session \
         id is {session_id}; if `aoe og` cannot detect you, pass `--as {session_id}`."
    )
}

pub fn orchestrator_bootstrap_prompt(
    group_path: &str,
    info: &OrchestrationInfo,
    memory: &Path,
) -> String {
    let brief = info
        .brief
        .as_deref()
        .unwrap_or("(no brief yet; ask the user)");
    format!(
        "You are the Orchestrator Agent for the aoe OrchestratedGroup '{group_path}'. Use the \
         `{SKILL_DIR}` skill. You plan, delegate to worker sessions with `aoe og spawn`, \
         review their results and keep project memory. Do not write code yourself.\n\n\
         Brief: {brief}\n\n\
         Memory directory: {}\nRead MEMORY.md there now. Then run `aoe og status` and \
         ask the user what to work on if the brief is not actionable. Board messages and \
         worker completions are delivered to you automatically; end your turn instead of polling.",
        memory.display()
    )
}

pub fn worker_task_prompt(group_path: &str, orchestrator_title: &str, task: &str) -> String {
    format!(
        "You are a worker in the aoe OrchestratedGroup '{group_path}', managed by the Orchestrator \
         Agent session '{orchestrator_title}'. Use the `{SKILL_DIR}` skill.\n\n\
         Task:\n{task}\n\n\
         When blocked, `aoe og post --kind question \"...\"` and end your turn; the answer will be \
         delivered to you. When done, `aoe og post --kind result \"<summary, branch, test status>\"` \
         exactly once, then end your turn."
    )
}

/// Appended to a worker's task when it starts in another worker's worktree.
pub fn shared_worktree_prompt(owner_title: &str) -> String {
    format!(
        "\n\nYou share this worktree with '{owner_title}'. Take turns editing: say in your \
         topic when you start and when you hand back (`aoe og post --topic <topic> \
         \"your turn: ...\"`), and do not edit while the other is working."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: &str, to: Option<&str>) -> BoardMessage {
        BoardMessage {
            kind: MessageKind::Note,
            from: from.into(),
            from_title: from.into(),
            to: to.map(Into::into),
            body: "hi".into(),
            at: Utc::now(),
            topic: None,
        }
    }

    fn info_with_topic() -> OrchestrationInfo {
        let mut info = OrchestrationInfo::new(None);
        info.orchestrator_session_id = Some("orchestrator".into());
        info.subscribe("review", "w1");
        info.subscribe("review", "rev");
        info
    }

    #[test]
    fn routing_and_cursor_delivery() {
        let tmp = tempfile::tempdir().unwrap();
        let mut board = Board::open_at(&tmp.path().join("b.db")).unwrap();
        let t = "topic";
        let info = info_with_topic();
        let in_review = |from: &str, to: Option<&str>| BoardMessage {
            topic: Some("review".into()),
            ..msg(from, to)
        };
        // (message, orchestrator, w1, w2, rev receive)
        let cases = [
            (msg("w1", None), true, false, false, false),
            (msg("orchestrator", Some("w1")), false, true, false, false),
            (
                msg("orchestrator", Some(ALL_WORKERS)),
                false,
                true,
                true,
                true,
            ),
            (msg("user", None), true, false, false, false),
            (msg("w2", Some(ALL_WORKERS)), false, true, false, true),
            (in_review("rev", None), false, true, false, false),
            (in_review("w1", None), false, false, false, true),
            (
                in_review("orchestrator", Some("rev")),
                false,
                false,
                false,
                true,
            ),
        ];
        for (m, ..) in &cases {
            board.post(t, m).unwrap();
        }
        for (reader, col) in [("orchestrator", 0), ("w1", 1), ("w2", 2), ("rev", 3)] {
            let want: Vec<u64> = cases
                .iter()
                .enumerate()
                .filter(|(_, c)| [c.1, c.2, c.3, c.4][col])
                .map(|(i, _)| i as u64 + 1)
                .collect();
            let (got, high) = board.pending_for(t, reader, &info).unwrap();
            assert_eq!(
                got.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
                want,
                "{reader}"
            );
            board.advance_cursor(t, reader, high).unwrap();
            assert!(board.pending_for(t, reader, &info).unwrap().0.is_empty());
        }
        board.advance_cursor(t, "w1", 1).unwrap();
        assert_eq!(board.cursor(t, "w1").unwrap(), 8);

        let mut info = info;
        info.unsubscribe("review", "rev");
        assert!(!info.subscribed("review", "rev") && info.subscribed("review", "w1"));
        info.unsubscribe("review", "w1");
        assert!(info.topics.is_empty());
    }

    #[test]
    fn delivery_text_is_one_line_per_message() {
        let mut result = msg("w1", None);
        result.kind = MessageKind::Result;
        assert_eq!(
            delivery_text(&[(7, msg("user", None))]),
            "[board] #7 from user: hi"
        );
        assert_eq!(
            delivery_text(&[(7, msg("user", None)), (8, result)]),
            "[board] 2 new messages\n#7 from user: hi\n#8 result from w1: hi"
        );
    }

    #[test]
    fn find_for_path_uses_nearest_project_ancestor() {
        let mut root = Group::new("p", "p");
        root.orchestration = Some(OrchestrationInfo::new(None));
        let groups = vec![root, Group::new("sub", "p/sub"), Group::new("q", "q")];
        assert_eq!(find_for_path(&groups, "p/sub").unwrap().path, "p");
        assert_eq!(find_for_path(&groups, "p").unwrap().path, "p");
        assert!(find_for_path(&groups, "q").is_none());
        assert!(find_for_path(&groups, "").is_none());
    }
}
