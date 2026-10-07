---
name: aoe-orchestrated-group
description: Coordinate an aoe OrchestratedGroup. Use when you are the Chief of Staff or a worker session in an aoe OrchestratedGroup, or when asked to delegate work to aoe worker sessions, post to or read the project board, or manage project groups.
---

# aoe OrchestratedGroup

A OrchestratedGroup is an aoe group with one Chief of Staff session and any number of
worker sessions. They coordinate through a durable message board. `aoe og`
commands infer your group and identity from the session you run them in.

Board messages addressed to you and worker completions are delivered into your
session automatically once you are idle, as lines starting with `[board]`. Reply
to them with `aoe og post`, not in chat. Never poll, sleep or loop waiting for
them: end your turn.

## Commands

| Command | Use |
|---|---|
| `aoe og status` | Group, chief, workers and their status, board size |
| `aoe og board [--after N] [--limit N]` | Read the board |
| `aoe og post --kind <kind> [--to <session or all>] "<text>"` | Post; no `--to` means the chief |
| `aoe og spawn --title <t> [--branch <b>] [--no-worktree] [--tool <agent>] [--structured-view] "<task>"` | Chief only: start a worker with a task |
| `aoe og memory` | Print the memory directory |
| `aoe session capture <worker>` | Read a worker's recent terminal output |
| `aoe group list`, `aoe group move <session> <group>` | Group management |
| `aoe session stop <worker>`, `aoe rm <worker>` | Retire a worker (ask the user before removing) |

Kinds: `task`, `status`, `question`, `answer`, `result`, `learning`, `note`.

## Chief of Staff

1. Read `MEMORY.md` in the memory directory at start and after long gaps.
2. Split work into independent tasks a worker can finish alone. Each task brief
   states the goal, acceptance criteria, relevant files and how to test. Workers
   do not see your conversation.
3. `aoe og spawn` one worker per task. Workers get their own worktree branch by
   default; keep parallel tasks off the same files.
4. End your turn. You will receive `[system]` messages when a worker goes idle,
   needs input or errors, plus any `question` or `result` posts.
5. Answer questions with `aoe og post --kind answer --to <worker> "..."`.
6. Review each `result`: check the branch diff and tests before reporting to the
   user. Send follow-ups with `--kind task --to <worker>`.
7. Curate memory: fold durable `learning` posts and decisions into topic files
   and keep `MEMORY.md` a short index of one-line pointers.
8. Do not edit project code yourself, and ask the user before merging, pushing
   or deleting sessions.

## Worker

1. Do the task you were given in your own worktree.
2. Blocked or unsure: `aoe og post --kind question "..."`, then end your turn.
3. Found something future workers should know (how to run tests, a gotcha):
   `aoe og post --kind learning "..."`.
4. Finished: `aoe og post --kind result "<summary, branch, tests run and outcome>"`
   exactly once, then end your turn.
