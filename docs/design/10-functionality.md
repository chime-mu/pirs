# Functionality — what a user can do

Status: draft. Written on 2026-09-20, revised the same day after the first review of it. It
is the acceptance test for the layers below it: if a scenario here cannot be walked through
with the architecture, protocol and DSL as written, the lower layer is wrong, not this one.

Each scenario names the phase (from `20-architecture.md`) that first makes it true. A phase
brief lists the scenarios it enables; when they all work, the phase is done.

## One-shot use

**S1 · Ask and get an answer.** (phase 1)
I type `pirs "why does the build fail?"` in a project directory. An agent is started for that
directory, the answer streams to my terminal, and the command exits. When it exits the agent
is closed; the conversation stays on disk. A server was started for me if none was running;
I never had to think about it, and a hundred one-shots leave nothing running.

**S2 · Several agents in one directory.** (phase 1)
Every `pirs "prompt"` starts a new agent unless I ask to continue a conversation, by name or
by picking from a list, which starts a fresh agent on it. I can have several agents working
in the same directory at once, each with its own conversation. The interactive UI shows the
agents that are running, and has a way to open past conversations; agents I start from the
UI live until I close them.

## Policy without code

**S3 · Customise without code.** (phase 2)
I put a short declaration file next to the project, or in my home directory for every
project, and pirs behaves differently. In pi each of these was an extension written in
TypeScript; here each is a few lines. The kinds:

- **Standing instructions** the model always gets, such as "prefer small commits, never
  rewrite history", or the contents of my existing rules files.
- **Live facts in the status line**, such as the current git branch, refreshed after every
  turn. Or a small panel showing my todo file, refreshed when the agent touches it.
- **Shortcuts on what I type.** A leading `?` asks for a brief answer; a leading `!` runs a
  shell command and never reaches the model; `/handoff` runs my own script and shows up in
  the command list with a description. Shortcuts and slash commands are the same thing with
  two spellings. Why these live with the agent rather than the UI is the test in S6.
- **Something that runs when something happens.** After every turn, git commits a
  checkpoint so I can undo whatever the agent did. On start, a file watcher is launched.

These files live with the server, next to the project it works in and in the home directory
of the machine it runs on. Locally that is my laptop and the question never comes up. For a
build box it means my personal policy has to be on the build box too; copying it there is a
later convenience, not something the first version does.

**S4 · Check before running.** (phase 2)
`pirs check` prints the merged policy for this directory, every conflict between files, and
the fully assembled system prompt the model would receive. Nothing is injected that I cannot
see.


**S5 · Ask the agent to extend pirs.** (phase 2 for declarations and scripts; phase 4 for processes)
This is the loop the whole design is measured by. I ask the agent for a change to how pirs
behaves; it writes the change into the project; the change is live on my next turn. No
build, no restart, nothing to install. The same three steps whatever the shape:

- **A declaration.** "Make `?` give me brief answers." The agent writes a few lines into a
  policy file. The server notices its own write and reloads.
- **A script.** "Give yourself a tool that fetches a URL." The agent writes `./tools/fetch.py`
  in whatever language, tests it from the shell with a line of JSON on stdin before pirs is
  involved at all, then adds the declaration that points at it.
- **A process.** "Watch the test output and tell me when it goes red." The agent writes a
  small program that connects to the server and registers for what it wants, starts it from
  the shell to try it, and adds the declaration that starts it with the agent from then on.

The agent has the two short documents it needs in front of it: the policy vocabulary and
the wire messages. On a remote server this works the same way, because the agent that
writes the file is on that machine. The UI is shaped the same way by an agent on my own
machine, which edits the UI's config file and the UI reloads; a machine that runs a UI can
always run an agent.

**S6 · Customise the UI.** (phase 3)
Theme, key bindings, the status line format, and which pages open where live in a config
file with the UI, on my laptop, never on the server. The test for where a customisation
goes: if I switched to a different UI, or drove the agent from a script, should it still
happen? If yes, it is the agent's and lives with the server as policy (S3). If it only makes
sense on this screen, it is the UI's. A customisation that needs both is two files.

Commands exist on both sides, placed by the same test. `/handoff` runs a script in the
project, so it is policy on the server. `/theme dark` changes how things look, so it is UI
config. `!cmd` runs in the project and its output enters the conversation where the model
can see it, so it is policy too; a `!cmd` that ran on my laptop would be a terminal, not an
agent feature. The UI shows one command list: the server's commands, learned when it
attaches to an agent, together with its own. A name defined on both sides is reported as a
conflict, never silently resolved. How far the UI itself can be extended is the UI's
business, with one requirement on the reference UI: it provides a client-side extension
point where a recognised tool call or content block is handed to my code to draw and to
take input from, so a convention such as structured questions (S12) is buildable without
writing a new UI. Nothing on the server changes for any of this.

## Policy with code

**S7 · Give the model a tool in any language.** (phase 4)
A declaration names `./tools/fetch.py`, describes its parameters, and the model can call it.
I can test the tool from the shell with `echo '{"url": ...}' | ./tools/fetch.py`; there is no
build step and no SDK.

**S8 · Replace or wrap a built-in.** (phase 4)
A declaration with the name of a built-in tool such as `bash` can disable it or wrap it in my
own script.

**S9 · A long-lived extension.** (phase 4)
A file watcher or a stateful tool runs as a process for the life of the agent, receives
events, and registers for the hooks it wants, in whatever language it is written in.

**S10 · Write the policy from a sentence.** (phase 7)
A convenience on top of S5 for when no agent is running: `pirs ext new "show the git branch
in the status line"` asks the model to write the declaration file, checks it, and shows me
the result. The sentence is stored in the file as
its intent, and `pirs ext regen` rebuilds the file from it later. The intent is what I share
with others, not the file.

## The interactive UI

**S11 · A sidebar of agents.** (phase 3)
`pirs tui` shows every agent on my server in a sidebar with its state. Selecting one shows its
conversation page. I can start another agent, in this directory or another, without leaving
the UI.

**S12 · Needs attention is a fact.** (phase 3)
The sidebar flags an agent that needs me: it has stopped, and I have not looked at it since.
Stopped is the server's fact; not looked at since is the UI's own. Neither reads what the
model wrote to guess whether it was a question. The only thing that ever asks me anything is
the model, in text, and I answer with a prompt. If I want questions with choices, I declare
an `ask` tool whose call carries the question and options as structured data, and a UI that
recognises it draws a picker (S6); nothing on the server changes.

**S13 · Read what the agent is editing.** (phase 3)
From an agent's page I open any file it touched this turn in a read-only file page. The page
refreshes while the agent keeps working. Several file pages can be open at once. This works
for a remote server too, because the server reads the file for me.

**S14 · Edit with my own editor.** (phase 3 inside tmux; S22 without it)
pirs never edits a file itself, and the UI never suspends. From a file page, a key opens
`$EDITOR` on the file in a terminal beside the UI. When the UI runs inside tmux it asks tmux
for a pane and runs the editor there, with ssh in front when the server is on another
machine or in a jail. Later the terminal page (S22) does the same without tmux. Local and
remote work the same way. This is the scenario that makes the terminal page more than a
nicety.

**S15 · Arrange the UI my way.** (any time after phase 3, client-side only)
Splits, tabs, windows and saved layouts are the UI's business. Adding them changes nothing
in the protocol and needs no server change.

## Several agents

**S16 · An agent that asks another agent.** (phase 5)
A declaration gives the model a `review` tool that starts a second agent with a different
model, waits for it to finish, and returns its answer. The second agent appears in the
sidebar like any other, so I can watch it.

**S17 · Wait for an agent from a script.** (phase 5)
A shell script can ask the server to block until an agent has stopped, so
orchestration outside pirs needs no polling.

## Remote and contained

**S18 · Agents on another machine.** (phase 6)
A config file names a build box. The sidebar shows its agents next to my local ones, a page
opens on `office:3` as easily as `local:1`, and files on that box are readable in file
pages. Transport is my existing SSH login; pirs opens no network port of its own.

**S19 · A dropped link.** (phase 6)
My SSH connection drops while an agent is working. When the UI reconnects it catches up on
what happened, including where the agent stopped while I was away.

**S20 · Run the agent in a jail.** (phase 6)
For a repository I do not trust, the server runs inside a container or VM and the UI stays on
my laptop. Every tool and extension executes inside the boundary. Attaching to a jailed agent
looks no different from a local one. pirs itself checks nothing and claims nothing; the
kernel, hypervisor or SSH is the boundary.

**S21 · Mixed machines.** (phase 6)
My laptop and my servers can run different operating systems, and two servers need not
match. Everything about an agent belongs to the server that runs it — its paths, its tools,
its policy — and the UI only shows these, never interprets them: a path from a Windows server
is a label the UI displays and hands back to that server, and files are read for me by the
server. Two servers on different pirs versions agree on a version when I connect, or refuse
cleanly. What is not portable is policy that names commands: a declaration with only text
and files works on any server, one that runs `git branch` works wherever `git` and a shell
exist. Promise for the first version: Linux and macOS servers; Linux, macOS or Windows as
the client; Windows as a server later, because shell strings need a decision about which
shell.

## Terminals

**S22 · A terminal page.** (phase 8, or earlier if S14 demands it)
If a pty server is present the UI can show terminal pages next to agent pages, including a
shell on the machine an agent runs on. Until then, tmux. The loop server never grows a
terminal.

## Another UI

**S23 · A web UI instead of the TUI.** (not planned; must be possible)
We decide to build a web UI. Three pieces: the page in the browser, which is the client and
speaks the same messages the TUI does over a websocket; a bridge, which serves the page and
forwards messages between the browser and the server, the same kind of thing as the ssh
proxy, and the one piece that opens a network port and therefore owns login; and the loop
server, unchanged. If building the web UI needs a change to the loop server, the protocol is
missing something. UI config lives with the bridge or the browser, so S5 still holds for it.
There is no tmux in a browser, so the web UI is where the terminal page (S22) stops being
optional.

## Deliberately not offered

- Any check inside the loop on what the model may do. See "What pirs is not" in
  `00-north-star.md` and D-19.
- Custom model providers with their own streaming, custom renderers, editors and overlays as
  extensions.
- pi extension compatibility.
