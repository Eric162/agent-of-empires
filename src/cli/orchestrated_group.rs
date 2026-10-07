//! `aoe orchestrated-group` (`aoe og`): OrchestratedGroup setup, worker spawning and the message board.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::{Args, Subcommand};
use serde::Serialize;

use super::definition::{Cli, Commands};
use crate::session::orchestrated_group::{
    self, Board, BoardMessage, MessageKind, OrchestrationInfo, ALL_WORKERS, SYSTEM_AUTHOR,
    USER_AUTHOR,
};
use crate::session::{Group, GroupTree, Instance, Storage};

#[derive(Subcommand)]
pub enum OrchestratedGroupCommands {
    /// Turn a group into an OrchestratedGroup and start its Orchestrator Agent session
    Init(InitArgs),
    /// Start a worker session with a task (run by the orchestrator)
    Spawn(SpawnArgs),
    /// Post a message to the board
    Post(PostArgs),
    /// Read the board
    Board(BoardArgs),
    /// Show the orchestrator, workers and board state
    Status(TargetArgs),
    /// Print the memory directory
    Memory(TargetArgs),
    /// Subscribe a session to a topic; topic posts reach only subscribers
    Sub(SubArgs),
    /// Unsubscribe a session from a topic
    Unsub(SubArgs),
    /// List topics and their subscribers
    Topics(TargetArgs),
}

#[derive(Args)]
pub struct SubArgs {
    topic: String,
    /// Session to (un)subscribe; defaults to yourself
    #[arg(long)]
    who: Option<String>,
    #[command(flatten)]
    target: TargetArgs,
}

#[derive(Args, Clone)]
pub struct TargetArgs {
    /// OrchestratedGroup path; defaults to the group of the session this runs in
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
    /// Skip permission prompts (defaults to your aoe setting)
    #[arg(long, short = 'y')]
    yolo: bool,
}

#[derive(Args)]
pub struct InitArgs {
    /// Group path to turn into an OrchestratedGroup (created if missing)
    group: String,
    /// Repository the orchestrator and its workers operate on (defaults to current directory)
    path: Option<PathBuf>,
    /// What this project is for
    #[arg(long)]
    brief: Option<String>,
    /// Orchestrator session title
    #[arg(long, short = 't')]
    title: Option<String>,
    /// Run the orchestrator in a worktree on this integration branch (created if missing);
    /// workers then branch from it
    #[arg(long, short = 'w')]
    worktree: Option<String>,
    /// Base for a newly created --worktree branch (defaults to the repo's default branch)
    #[arg(long = "base-branch", requires = "worktree")]
    base_branch: Option<String>,
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
    /// Worktree branch (defaults to og/<title>)
    #[arg(long, short = 'b')]
    branch: Option<String>,
    /// Run in the orchestrator's checkout instead of a new worktree
    #[arg(long = "no-worktree", conflicts_with = "branch")]
    no_worktree: bool,
    /// Work in this worker's worktree instead of a new one (pairing, review)
    #[arg(long = "in", conflicts_with_all = ["branch", "no_worktree"])]
    in_worker: Option<String>,
    /// Run in a fresh scratch directory with no repository
    #[arg(long, conflicts_with_all = ["branch", "no_worktree", "in_worker"])]
    scratch: bool,
    /// Subscribe the worker to this topic and tag its task with it
    #[arg(long)]
    topic: Option<String>,
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
    /// Recipient session (id or title), `all` for every worker; defaults to the orchestrator
    #[arg(long)]
    to: Option<String>,
    /// Tag the post with a topic; it then reaches the topic's subscribers
    #[arg(long)]
    topic: Option<String>,
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
pub async fn run(profile: &str, command: OrchestratedGroupCommands) -> Result<()> {
    match command {
        OrchestratedGroupCommands::Init(args) => init(profile, args).await,
        OrchestratedGroupCommands::Spawn(args) => spawn(profile, args).await,
        OrchestratedGroupCommands::Post(args) => post(profile, args),
        OrchestratedGroupCommands::Board(args) => board(profile, args),
        OrchestratedGroupCommands::Status(args) => status(profile, args),
        OrchestratedGroupCommands::Memory(args) => {
            let ctx = Ctx::resolve(profile, &args)?;
            println!("{}", orchestrated_group::memory_dir(&ctx.info)?.display());
            Ok(())
        }
        OrchestratedGroupCommands::Sub(args) => subscription(profile, args, true),
        OrchestratedGroupCommands::Unsub(args) => subscription(profile, args, false),
        OrchestratedGroupCommands::Topics(args) => {
            print_topics(&Ctx::resolve(profile, &args)?);
            Ok(())
        }
    }
}

/// The OrchestratedGroup a command acts on, and who is acting.
struct Ctx {
    profile: String,
    group: Group,
    info: OrchestrationInfo,
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
        let group = orchestrated_group::find_for_path(&groups, &group_path)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "'{group_path}' is not in an OrchestratedGroup; run `aoe og init <group>` first"
                )
            })?;
        let info = group
            .orchestration
            .clone()
            .expect("find_for_path filters on orchestration");
        Ok(Self {
            profile,
            group,
            info,
            instances,
            me,
        })
    }

    fn orchestrator_id(&self) -> &str {
        self.info
            .orchestrator_session_id
            .as_deref()
            .unwrap_or_default()
    }

    fn orchestrator(&self) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|i| i.id == self.orchestrator_id())
    }

    fn member(&self, who: &str) -> Result<Instance> {
        let members: Vec<Instance> = orchestrated_group::members(&self.group, &self.instances)
            .into_iter()
            .cloned()
            .collect();
        super::resolve_session(who, &members).cloned()
    }

    fn title_of(&self, id: &str) -> String {
        self.instances
            .iter()
            .find(|i| i.id == id)
            .map_or_else(|| id.to_string(), |i| i.title.clone())
    }

    fn post(
        &self,
        kind: MessageKind,
        to: Option<String>,
        topic: Option<String>,
        body: String,
    ) -> Result<u64> {
        let (from, from_title) = if kind == MessageKind::System {
            (SYSTEM_AUTHOR.to_string(), SYSTEM_AUTHOR.to_string())
        } else {
            self.author()
        };
        Board::open()?.post(
            &self.info.id,
            &BoardMessage {
                kind,
                from,
                from_title,
                to,
                body,
                at: Utc::now(),
                topic,
            },
        )
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
    if agent.yolo {
        out.push("-y".into());
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
    f: impl FnOnce(&mut OrchestrationInfo),
) -> Result<()> {
    Storage::open_unwatched(profile)?.update(|_, groups| {
        let g = groups
            .iter_mut()
            .find(|g| g.path == group_path)
            .ok_or_else(|| anyhow!("group '{group_path}' disappeared"))?;
        f(g.orchestration
            .as_mut()
            .ok_or_else(|| anyhow!("not an OrchestratedGroup"))?);
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
    orchestrated_group::install_skill().context("install the aoe-orchestrated-group skill")?;

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
            .orchestration
            .get_or_insert_with(|| OrchestrationInfo::new(args.brief.clone()));
        if args.brief.is_some() {
            info.brief = args.brief.clone();
        }
        Ok(info.clone())
    })?;

    if let Some(orchestrator) = info.orchestrator_session_id.as_deref() {
        let instances = storage.load()?;
        if let Some(existing) = instances
            .iter()
            .find(|i| i.id == orchestrator && !i.is_trashed() && !i.is_archived())
        {
            bail!(
                "'{group_path}' already has an Orchestrator Agent: '{}' ({})",
                existing.title,
                existing.id
            );
        }
    }

    let memory = orchestrated_group::ensure_memory(&info, &group_path)?;
    let title = args.title.unwrap_or_else(|| {
        format!(
            "{} orchestrator",
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
    if let Some(branch) = &args.worktree {
        let repo = crate::git::GitWorktree::new(path.canonicalize()?)?;
        add.extend(["-w".into(), branch.clone()]);
        if !repo.branch_exists(branch)? {
            add.push("-b".into());
            if let Some(base) = &args.base_branch {
                add.extend(["--base-branch".into(), base.clone()]);
            }
        }
    }
    add.extend(agent_flags(&args.agent));
    add_session(profile, add).await?;
    let orchestrator = newest_session(profile, &title, &group_path)?;
    update_info(profile, &group_path, |i| {
        i.orchestrator_session_id = Some(orchestrator.id.clone())
    })?;

    let mut board = Board::open()?;
    board.post(
        &info.id,
        &BoardMessage {
            kind: MessageKind::System,
            from: orchestrated_group::SYSTEM_AUTHOR.into(),
            from_title: orchestrated_group::SYSTEM_AUTHOR.into(),
            to: Some(orchestrator.id.clone()),
            body: format!("OrchestratedGroup '{group_path}' started with orchestrator '{title}'"),
            at: Utc::now(),
            topic: None,
        },
    )?;
    board.advance_cursor(&info.id, &orchestrator.id, board.highest_seq(&info.id))?;

    let prompt = format!(
        "{}\n\n{}",
        orchestrated_group::orchestrator_bootstrap_prompt(&group_path, &info, &memory),
        orchestrated_group::identity_footer(&orchestrator.id)
    );
    send_to(profile, &orchestrator.id, &prompt).await?;

    println!("OrchestratedGroup '{group_path}' ready");
    println!("  Orchestrator:  {title} ({})", orchestrator.id);
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
    let orchestrator = ctx.orchestrator().cloned().ok_or_else(|| {
        anyhow!(
            "OrchestratedGroup '{}' has no live orchestrator",
            ctx.group.path
        )
    })?;
    if let Some(me) = &ctx.me {
        if me.id != orchestrator.id {
            bail!("only the Orchestrator Agent spawns workers; post a `question` to ask for one");
        }
    }

    // From the main checkout so worker worktrees sit beside the orchestrator's, not inside it.
    let repo_path = orchestrator
        .worktree_info
        .as_ref()
        .map_or(orchestrator.project_path.clone(), |wt| {
            wt.main_repo_path.clone()
        });
    let sharing = args
        .in_worker
        .as_deref()
        .map(|who| ctx.member(who))
        .transpose()?;
    let path = match (&sharing, args.scratch) {
        (Some(owner), _) => owner.project_path.clone(),
        (None, true) => "--scratch".into(),
        (None, false) => repo_path,
    };
    let mut add = vec![
        path,
        "-t".into(),
        args.title.clone(),
        "-P".into(),
        orchestrator.id.clone(),
        "-l".into(),
    ];
    if !args.no_worktree && sharing.is_none() && !args.scratch {
        let branch = args
            .branch
            .clone()
            .unwrap_or_else(|| format!("og/{}", slug(&args.title)));
        add.extend(["-w".into(), branch, "-b".into()]);
        if let Some(wt) = &orchestrator.worktree_info {
            add.extend(["--base-branch".into(), wt.branch.clone()]);
        }
    }
    add.extend(agent_flags(&args.agent));
    add_session(&ctx.profile, add).await?;
    let worker = newest_session(&ctx.profile, &args.title, &orchestrator.group_path)?;
    let topic = args.topic.as_deref().map(slug);
    if let Some(topic) = &topic {
        let (worker_id, orchestrator_id) = (worker.id.clone(), orchestrator.id.clone());
        update_info(&ctx.profile, &ctx.group.path, |info| {
            info.subscribe(topic, &worker_id);
            info.subscribe(topic, &orchestrator_id);
            if let Some(owner) = &sharing {
                info.subscribe(topic, &owner.id);
            }
        })?;
    }

    let mut board = Board::open()?;
    let seq = board.post(
        &ctx.info.id,
        &BoardMessage {
            kind: MessageKind::Task,
            from: orchestrator.id.clone(),
            from_title: orchestrator.title.clone(),
            to: Some(worker.id.clone()),
            body: args.task.clone(),
            at: Utc::now(),
            topic: topic.clone(),
        },
    )?;
    // The task goes out directly below, so the relay must not deliver it again.
    board.advance_cursor(&ctx.info.id, &worker.id, seq)?;

    let mut prompt =
        orchestrated_group::worker_task_prompt(&ctx.group.path, &orchestrator.title, &args.task);
    if let Some(owner) = &sharing {
        prompt.push_str(&orchestrated_group::shared_worktree_prompt(&owner.title));
    }
    if let Some(topic) = &topic {
        prompt.push_str(&format!(
            "\n\nYou are subscribed to #{topic}; discuss this task there with \
             `aoe og post --topic {topic}`."
        ));
    }
    let prompt = format!(
        "{prompt}\n\n{}",
        orchestrated_group::identity_footer(&worker.id)
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
        None | Some("orchestrator") => None,
        Some(ALL_WORKERS) => Some(ALL_WORKERS.to_string()),
        Some(who) => {
            let members: Vec<Instance> = orchestrated_group::members(&ctx.group, &ctx.instances)
                .into_iter()
                .cloned()
                .collect();
            let target = super::resolve_session(who, &members)?;
            (target.id != ctx.orchestrator_id()).then(|| target.id.clone())
        }
    };
    let topic = args.topic.as_deref().map(slug);
    let seq = ctx.post(args.kind, to, topic.clone(), args.body)?;
    match topic {
        Some(t) => println!("Posted #{seq} in #{t}"),
        None => println!("Posted #{seq}"),
    }
    Ok(())
}

fn subscription(profile: &str, args: SubArgs, subscribe: bool) -> Result<()> {
    let ctx = Ctx::resolve(profile, &args.target)?;
    let who = match args.who.as_deref() {
        Some(who) => ctx.member(who)?,
        None => ctx
            .me
            .clone()
            .ok_or_else(|| anyhow!("pass --who <session> when not running inside one"))?,
    };
    let topic = slug(&args.topic);
    if topic.is_empty() {
        bail!("topic name cannot be empty");
    }
    let id = who.id.clone();
    update_info(&ctx.profile, &ctx.group.path, |info| {
        if subscribe {
            info.subscribe(&topic, &id);
        } else {
            info.unsubscribe(&topic, &id);
        }
    })?;
    let verb = if subscribe {
        "Subscribed"
    } else {
        "Unsubscribed"
    };
    println!(
        "{verb} {} {} #{topic}",
        who.title,
        if subscribe { "to" } else { "from" }
    );
    Ok(())
}

fn print_topics(ctx: &Ctx) {
    if ctx.info.topics.is_empty() {
        println!("(no topics)");
    }
    for (topic, subs) in &ctx.info.topics {
        let names: Vec<String> = subs.iter().map(|s| ctx.title_of(s)).collect();
        println!("#{topic}  {}", names.join(", "));
    }
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
    println!("OrchestratedGroup: {}", ctx.group.path);
    if let Some(brief) = &ctx.info.brief {
        println!("Brief:        {brief}");
    }
    println!(
        "Memory:       {}",
        orchestrated_group::memory_dir(&ctx.info)?.display()
    );
    println!("Board:        {} messages", board.highest_seq(&ctx.info.id));
    match ctx.orchestrator() {
        Some(c) => println!("Orchestrator:        {} ({}) {:?}", c.title, c.id, c.status),
        None => println!("Orchestrator:        (none)"),
    }
    let workers: Vec<&Instance> = orchestrated_group::members(&ctx.group, &ctx.instances)
        .into_iter()
        .filter(|i| i.id != ctx.orchestrator_id())
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
    if !ctx.info.topics.is_empty() {
        println!("Topics:");
        print_topics(&ctx);
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
