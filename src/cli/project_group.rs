//! `aoe project-group` (`aoe pg`): ProjectGroup setup, worker spawning and the message board.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use super::definition::{Cli, Commands};
use crate::session::project_group::{
    self, Board, BoardMessage, MessageKind, ProjectGroupInfo, ALL_WORKERS, USER_AUTHOR,
};
use crate::session::{Group, GroupTree, Instance, Storage};

#[derive(Subcommand)]
pub enum ProjectGroupCommands {
    /// Turn a group into a ProjectGroup and start its Chief of Staff session
    Init(InitArgs),
    /// Start a worker session with a task (run by the chief)
    Spawn(SpawnArgs),
    /// Post a message to the board
    Post(PostArgs),
    /// Read the board
    Board(BoardArgs),
    /// Show the chief, workers and board state
    Status(TargetArgs),
    /// Print the memory directory
    Memory(TargetArgs),
}

#[derive(Args, Clone)]
pub struct TargetArgs {
    /// ProjectGroup path; defaults to the group of the session this runs in
    #[arg(long, short = 'g')]
    group: Option<String>,
    /// Act as this session (id or title) instead of detecting the current one
    #[arg(long = "as")]
    as_session: Option<String>,
}

#[derive(Args, Clone, Default)]
pub struct AgentArgs {
    /// Named agent to run (defaults to your aoe default)
    #[arg(long)]
    tool: Option<String>,
    /// Use the structured (ACP) view instead of a terminal session
    #[arg(long = "structured-view")]
    structured_view: bool,
    /// ACP agent for the structured view
    #[arg(long)]
    agent: Option<String>,
    /// Model override
    #[arg(long)]
    model: Option<String>,
}

#[derive(Args)]
pub struct InitArgs {
    /// Group path to turn into a ProjectGroup (created if missing)
    group: String,
    /// Repository the chief and its workers operate on (defaults to current directory)
    path: Option<PathBuf>,
    /// What this project is for
    #[arg(long)]
    brief: Option<String>,
    /// Chief session title
    #[arg(long, short = 't')]
    title: Option<String>,
    #[command(flatten)]
    agent: AgentArgs,
}

#[derive(Args)]
pub struct SpawnArgs {
    /// Task brief for the worker
    task: String,
    /// Worker session title
    #[arg(long, short = 't')]
    title: String,
    /// Worktree branch (defaults to pg/<title>)
    #[arg(long, short = 'b')]
    branch: Option<String>,
    /// Run in the chief's checkout instead of a new worktree
    #[arg(long = "no-worktree", conflicts_with = "branch")]
    no_worktree: bool,
    #[command(flatten)]
    agent: AgentArgs,
    #[command(flatten)]
    target: TargetArgs,
}

#[derive(Args)]
pub struct PostArgs {
    /// Message text
    body: String,
    #[arg(long, short = 'k', value_enum, default_value = "note")]
    kind: MessageKind,
    /// Recipient session (id or title), `all` for every worker; defaults to the chief
    #[arg(long)]
    to: Option<String>,
    #[command(flatten)]
    target: TargetArgs,
}

#[derive(Args)]
pub struct BoardArgs {
    /// Only messages after this seq
    #[arg(long, default_value_t = 0)]
    after: u64,
    /// Only the last N messages
    #[arg(long)]
    limit: Option<usize>,
    #[arg(long)]
    json: bool,
    #[command(flatten)]
    target: TargetArgs,
}

#[tracing::instrument(target = "cli.session", skip_all, fields(profile = %profile))]
pub async fn run(profile: &str, command: ProjectGroupCommands) -> Result<()> {
    match command {
        ProjectGroupCommands::Init(args) => init(profile, args).await,
        ProjectGroupCommands::Spawn(args) => spawn(profile, args).await,
        ProjectGroupCommands::Post(args) => post(profile, args),
        ProjectGroupCommands::Board(args) => board(profile, args),
        ProjectGroupCommands::Status(args) => status(profile, args),
        ProjectGroupCommands::Memory(args) => {
            let ctx = Ctx::resolve(profile, &args)?;
            println!("{}", project_group::memory_dir(&ctx.info)?.display());
            Ok(())
        }
    }
}

/// The ProjectGroup a command acts on, and who is acting.
struct Ctx {
    profile: String,
    group: Group,
    info: ProjectGroupInfo,
    instances: Vec<Instance>,
    me: Option<Instance>,
}

impl Ctx {
    fn resolve(profile: &str, target: &TargetArgs) -> Result<Self> {
        let (profile, me_id) = match (&target.group, &target.as_session) {
            (Some(_), None) => (profile.to_string(), None),
            (_, Some(who)) => (profile.to_string(), Some(who.clone())),
            (None, None) => detect_current_session(profile)?,
        };
        let storage = Storage::open_unwatched(&profile)?;
        let (instances, groups) = storage.load_with_groups()?;
        let me = me_id
            .map(|id| super::resolve_session(&id, &instances).cloned())
            .transpose()?;
        let group_path = match (&target.group, &me) {
            (Some(g), _) => g.clone(),
            (None, Some(me)) => me.group_path.clone(),
            (None, None) => unreachable!("detect_current_session returns a session"),
        };
        let group = project_group::find_for_path(&groups, &group_path)
            .cloned()
            .ok_or_else(|| {
                anyhow!("'{group_path}' is not in a ProjectGroup; run `aoe pg init <group>` first")
            })?;
        let info = group
            .project
            .clone()
            .expect("find_for_path filters on project");
        Ok(Self {
            profile,
            group,
            info,
            instances,
            me,
        })
    }

    fn chief_id(&self) -> &str {
        self.info.chief_session_id.as_deref().unwrap_or_default()
    }

    fn chief(&self) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == self.chief_id())
    }

    fn author(&self) -> (String, String) {
        match &self.me {
            Some(me) => (me.id.clone(), me.title.clone()),
            None => (USER_AUTHOR.to_string(), USER_AUTHOR.to_string()),
        }
    }
}

/// `AOE_INSTANCE_ID` is set for agents with hooks; otherwise fall back to the tmux session.
fn detect_current_session(profile: &str) -> Result<(String, Option<String>)> {
    let env_id = std::env::var(crate::tmux::env::AOE_INSTANCE_ID_KEY)
        .ok()
        .filter(|s| !s.is_empty());
    let tmux_name = std::env::var("TMUX_PANE")
        .ok()
        .and_then(|_| crate::tmux::get_current_session_name());
    if env_id.is_none() && tmux_name.is_none() {
        bail!("not running inside an aoe session; pass --group <path> or --as <session>");
    }
    let profiles = if profile.is_empty() {
        crate::session::list_profiles()?
    } else {
        vec![profile.to_string()]
    };
    for p in profiles {
        let Ok((instances, _)) = Storage::open_unwatched(&p).and_then(|s| s.load_with_groups())
        else {
            continue;
        };
        let found = instances.iter().find(|i| {
            env_id.as_deref() == Some(i.id.as_str())
                || tmux_name
                    .as_deref()
                    .is_some_and(|n| crate::tmux::agent_session_belongs_to(n, &i.id))
        });
        if let Some(inst) = found {
            return Ok((p, Some(inst.id.clone())));
        }
    }
    bail!("current session is not an aoe session; pass --group <path> or --as <session>")
}

fn agent_flags(agent: &AgentArgs) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(t) = &agent.tool {
        out.extend(["--tool".into(), t.clone()]);
    }
    if agent.structured_view {
        out.push("--structured-view".into());
    }
    if let Some(a) = &agent.agent {
        out.extend(["--agent".into(), a.clone()]);
    }
    if let Some(m) = &agent.model {
        out.extend(["--model".into(), m.clone()]);
    }
    out
}

/// Run `aoe add` in-process so workers get exactly the user's configured defaults.
async fn add_session(profile: &str, args: Vec<String>) -> Result<()> {
    let argv = ["aoe".to_string(), "add".to_string()]
        .into_iter()
        .chain(args);
    let cli = <Cli as clap::Parser>::try_parse_from(argv)?;
    let Some(Commands::Add(add)) = cli.command else {
        unreachable!("parsed an add command");
    };
    super::add::run(profile, *add).await
}

async fn send_to(profile: &str, session_id: &str, message: &str) -> Result<()> {
    let cli = <Cli as clap::Parser>::try_parse_from(["aoe", "send", session_id, message])?;
    let Some(Commands::Send(send)) = cli.command else {
        unreachable!("parsed a send command");
    };
    super::send::run(profile, send).await
}

fn newest_session(profile: &str, title: &str, group_path: &str) -> Result<Instance> {
    let instances = Storage::open_unwatched(profile)?.load()?;
    instances
        .into_iter()
        .filter(|i| i.title == title && i.group_path == group_path)
        .max_by_key(|i| i.created_at)
        .ok_or_else(|| anyhow!("created session '{title}' not found in '{group_path}'"))
}

fn update_info(
    profile: &str,
    group_path: &str,
    f: impl FnOnce(&mut ProjectGroupInfo),
) -> Result<()> {
    Storage::open_unwatched(profile)?.update(|_, groups| {
        let g = groups
            .iter_mut()
            .find(|g| g.path == group_path)
            .ok_or_else(|| anyhow!("group '{group_path}' disappeared"))?;
        f(g.project
            .as_mut()
            .ok_or_else(|| anyhow!("not a ProjectGroup"))?);
        Ok(())
    })
}

async fn warn_without_daemon() {
    if crate::acp::client::require_daemon().await.is_err() {
        eprintln!(
            "warning: no aoe daemon is running, so board messages and worker completions \
             will not be delivered. Start one with `aoe serve --daemon`."
        );
    }
}

fn warn_without_pre_trust(profile: &str) {
    let config = crate::session::config::profile_config::resolve_config_or_warn(profile);
    if !config.session.pre_trust_agent_folders {
        eprintln!(
            "warning: session.pre_trust_agent_folders is off, so agents such as Claude may open \
             new worktrees on a folder-trust prompt that delivered tasks will dismiss."
        );
    }
}

async fn init(profile: &str, args: InitArgs) -> Result<()> {
    crate::session::require_known_profile(profile)?;
    let group_path = args.group.trim_matches('/').to_string();
    if group_path.is_empty() {
        bail!("group path cannot be empty");
    }
    project_group::install_skill().context("install the aoe-project-group skill")?;

    let storage = Storage::open_unwatched(profile)?;
    let info = storage.update(|instances, groups| {
        let mut tree = GroupTree::new_with_groups(instances, groups);
        tree.create_group(&group_path);
        *groups = tree.get_all_groups();
        let g = groups
            .iter_mut()
            .find(|g| g.path == group_path)
            .expect("just created");
        let info = g
            .project
            .get_or_insert_with(|| ProjectGroupInfo::new(args.brief.clone()));
        if args.brief.is_some() {
            info.brief = args.brief.clone();
        }
        Ok(info.clone())
    })?;

    if let Some(chief) = info.chief_session_id.as_deref() {
        let instances = storage.load()?;
        if let Some(existing) = instances
            .iter()
            .find(|i| i.id == chief && !i.is_trashed() && !i.is_archived())
        {
            bail!(
                "'{group_path}' already has a Chief of Staff: '{}' ({})",
                existing.title,
                existing.id
            );
        }
    }

    let memory = project_group::ensure_memory(&info, &group_path)?;
    let title = args.title.unwrap_or_else(|| {
        format!(
            "{} chief",
            group_path.rsplit('/').next().unwrap_or(&group_path)
        )
    });
    let path = args.path.unwrap_or_else(|| PathBuf::from("."));

    let mut add = vec![
        path.to_string_lossy().into_owned(),
        "-t".into(),
        title.clone(),
        "-g".into(),
        group_path.clone(),
        "-l".into(),
    ];
    add.extend(agent_flags(&args.agent));
    add_session(profile, add).await?;
    let chief = newest_session(profile, &title, &group_path)?;
    update_info(profile, &group_path, |i| {
        i.chief_session_id = Some(chief.id.clone())
    })?;

    let mut board = Board::open()?;
    board.post(
        &info.id,
        &BoardMessage {
            kind: MessageKind::System,
            from: project_group::SYSTEM_AUTHOR.into(),
            from_title: project_group::SYSTEM_AUTHOR.into(),
            to: Some(chief.id.clone()),
            body: format!("ProjectGroup '{group_path}' started with chief '{title}'"),
            at: Utc::now(),
        },
    )?;
    board.advance_cursor(&info.id, &chief.id, board.highest_seq(&info.id))?;

    let prompt = format!(
        "{}\n\n{}",
        project_group::chief_bootstrap_prompt(&group_path, &info, &memory),
        project_group::identity_footer(&chief.id)
    );
    send_to(profile, &chief.id, &prompt).await?;

    println!("ProjectGroup '{group_path}' ready");
    println!("  Chief:  {title} ({})", chief.id);
    println!("  Memory: {}", memory.display());
    warn_without_daemon().await;
    warn_without_pre_trust(profile);
    Ok(())
}

fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    out.split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

async fn spawn(profile: &str, args: SpawnArgs) -> Result<()> {
    let ctx = Ctx::resolve(profile, &args.target)?;
    let chief = ctx
        .chief()
        .cloned()
        .ok_or_else(|| anyhow!("ProjectGroup '{}' has no live chief", ctx.group.path))?;
    if let Some(me) = &ctx.me {
        if me.id != chief.id {
            bail!("only the Chief of Staff spawns workers; post a `question` to ask for one");
        }
    }

    let mut add = vec![
        chief.project_path.clone(),
        "-t".into(),
        args.title.clone(),
        "-P".into(),
        chief.id.clone(),
        "-l".into(),
    ];
    if !args.no_worktree {
        let branch = args
            .branch
            .clone()
            .unwrap_or_else(|| format!("pg/{}", slug(&args.title)));
        add.extend(["-w".into(), branch, "-b".into()]);
    }
    add.extend(agent_flags(&args.agent));
    add_session(&ctx.profile, add).await?;
    let worker = newest_session(&ctx.profile, &args.title, &chief.group_path)?;

    let mut board = Board::open()?;
    let seq = board.post(
        &ctx.info.id,
        &BoardMessage {
            kind: MessageKind::Task,
            from: chief.id.clone(),
            from_title: chief.title.clone(),
            to: Some(worker.id.clone()),
            body: args.task.clone(),
            at: Utc::now(),
        },
    )?;
    // The task goes out directly below, so the relay must not deliver it again.
    board.advance_cursor(&ctx.info.id, &worker.id, seq)?;

    let prompt = format!(
        "{}\n\n{}",
        project_group::worker_task_prompt(&ctx.group.path, &chief.title, &args.task),
        project_group::identity_footer(&worker.id)
    );
    send_to(&ctx.profile, &worker.id, &prompt).await?;
    println!(
        "Worker '{}' ({}) started with task #{seq}",
        worker.title, worker.id
    );
    Ok(())
}

fn post(profile: &str, args: PostArgs) -> Result<()> {
    let ctx = Ctx::resolve(profile, &args.target)?;
    let to = match args.to.as_deref() {
        None | Some("chief") => None,
        Some(ALL_WORKERS) => Some(ALL_WORKERS.to_string()),
        Some(who) => {
            let members: Vec<Instance> = project_group::members(&ctx.group, &ctx.instances)
                .into_iter()
                .cloned()
                .collect();
            let target = super::resolve_session(who, &members)?;
            (target.id != ctx.chief_id()).then(|| target.id.clone())
        }
    };
    let (from, from_title) = ctx.author();
    let seq = Board::open()?.post(
        &ctx.info.id,
        &BoardMessage {
            kind: args.kind,
            from,
            from_title,
            to,
            body: args.body,
            at: Utc::now(),
        },
    )?;
    println!("Posted #{seq}");
    Ok(())
}

fn board(profile: &str, args: BoardArgs) -> Result<()> {
    let ctx = Ctx::resolve(profile, &args.target)?;
    let board = Board::open()?;
    let mut msgs = board.read(&ctx.info.id, args.after, None);
    if let Some(n) = args.limit {
        msgs = msgs.split_off(msgs.len().saturating_sub(n));
    }
    if args.json {
        #[derive(Serialize)]
        struct Row<'a> {
            seq: u64,
            #[serde(flatten)]
            msg: &'a BoardMessage,
        }
        let rows: Vec<Row> = msgs
            .iter()
            .map(|(seq, msg)| Row { seq: *seq, msg })
            .collect();
        return super::output::print_json(&rows);
    }
    if msgs.is_empty() {
        println!("(board is empty)");
    }
    for (seq, m) in &msgs {
        println!("{}  {}", m.at.format("%H:%M:%S"), m.render(*seq));
    }
    Ok(())
}

fn status(profile: &str, args: TargetArgs) -> Result<()> {
    let ctx = Ctx::resolve(profile, &args)?;
    let board = Board::open()?;
    println!("ProjectGroup: {}", ctx.group.path);
    if let Some(brief) = &ctx.info.brief {
        println!("Brief:        {brief}");
    }
    println!(
        "Memory:       {}",
        project_group::memory_dir(&ctx.info)?.display()
    );
    println!("Board:        {} messages", board.highest_seq(&ctx.info.id));
    match ctx.chief() {
        Some(c) => println!("Chief:        {} ({}) {:?}", c.title, c.id, c.status),
        None => println!("Chief:        (none)"),
    }
    let workers: Vec<&Instance> = project_group::members(&ctx.group, &ctx.instances)
        .into_iter()
        .filter(|i| i.id != ctx.chief_id())
        .collect();
    println!("Workers:      {}", workers.len());
    for w in workers {
        let branch = w
            .worktree_info
            .as_ref()
            .map(|wt| format!(" [{}]", wt.branch))
            .unwrap_or_default();
        println!("  {} ({}) {:?}{branch}", w.title, w.id, w.status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn slug_makes_branch_safe_names() {
        for (input, want) in [("Fix Login!", "fix-login"), ("  a__b  ", "a-b"), ("x", "x")] {
            assert_eq!(super::slug(input), want);
        }
    }
}
