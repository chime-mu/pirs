# pirs status

Last updated: 2026-09-20.

pirs is a Rust port of [pi](https://github.com/earendil-works/pi). The goal of the first
milestone was a working coding agent with pi's architecture and, above all, proof that pi's
TypeScript extension model can run from a Rust host. Both are done.

## Verification state

- `cargo test --workspace`: 89 tests pass (pi-ai 11, pi-agent 2, pi-cli 71, pi-ext 5).
- Extension compatibility sweep (`cargo run -p pi-ext --example sweep -- <pi>/packages/coding-agent/examples/extensions`):
  71 of 77 pi example extensions load unchanged. The other 6 need npm packages not installed
  in the checkout (`@anthropic-ai/sdk`, `ms`, `@earendil-works/gondolin`,
  `@anthropic-ai/sandbox-runtime`) or Node streams (`fs.createReadStream`, `node:zlib`).
- End-to-end runs (faux provider, print mode, JSON mode, session continue, interactive TUI
  driven through tmux) exercised `hello.ts`, `permission-gate.ts`, `protected-paths.ts`,
  `dynamic-tools.ts`, `todo.ts` against the real agent loop and persisted the results.
- Live Anthropic requests verified on 2026-09-18 through the Claude Code keychain login
  (`pirs -p --model anthropic/claude-sonnet-4-5 "say hi in five words"` answered). A stale
  `~/.claude/.credentials.json` is skipped in favour of the keychain entry.
- Handoff notes for the next session are in `HANDOFF.md`.
- Not verified: live OpenAI requests (no key on the development machine); covered only by
  request-shape unit tests. The faux provider (`--model faux/scripted`,
  `PIRS_FAUX_SCRIPT=<json>`) stands in for real models in tests.

## Implemented

### pi-ai
- Message, content, usage, cost, model, tool types with pi's exact JSON shapes (session files interoperate).
- Anthropic Messages streaming: system prompt with cache control, tool use/results, thinking
  (budget based), redacted thinking, usage and cost, stop reason mapping, abort.
- OpenAI Chat Completions streaming (also for OpenAI-compatible servers): developer/system role,
  tool calls, `reasoning_content`, `reasoning_effort`, usage with cached tokens, abort.
- SSE parser, truncated-JSON salvage for tool arguments.
- Model registry with a built-in catalog, `models.json` providers (`$ENV` and `!command` keys),
  `registerProvider` from extensions, credential resolution from env vars.
- Anthropic OAuth: pi's `auth.json` and the Claude Code login (`~/.claude/.credentials.json` or
  the macOS keychain) are used when no API key is set; expired file-based tokens are refreshed
  and written back; requests use pi's Claude Code conventions (Bearer auth, beta flags, identity
  system block, tool-name casing). Verified live against the Anthropic API.
- Faux scripted provider.

### pi-agent
- `AgentTool` trait, `ToolResult`, `AgentMessage` union (system, user, assistant, toolResult,
  custom, bashExecution, branchSummary, compactionSummary).
- Agent loop ported from `agent-loop.ts`: sequential and parallel tool execution, argument
  validation, `beforeToolCall`/`afterToolCall` hooks, truncated-output failure handling,
  early termination, steering and follow-up queues, event protocol
  (`agent_start`, `turn_start`, `message_*`, `tool_execution_*`, `turn_end`, `agent_end`).
- `Agent` wrapper with shared state, listeners, abort, wait-for-idle.

### pi-ext (extension host)
- Dedicated thread with an rquickjs `AsyncRuntime`; requests are spawned onto the JS scheduler so
  extension calls interleave (a command awaiting `waitForIdle` does not block `tool_call` dispatch).
- TypeScript stripping via `swc_ts_fast_strip`, with full transform fallback.
- Module resolver: relative imports, `index.ts` directories, `package.json` `pi.extensions`,
  ESM packages in `node_modules`, and embedded virtual modules for `typebox`,
  `@earendil-works/pi-ai`, `@earendil-works/pi-coding-agent`, `@earendil-works/pi-agent-core`,
  `@earendil-works/pi-tui`, and `node:fs`, `fs/promises`, `path`, `os`, `child_process`, `util`,
  `url`, `crypto`, `process`, `events`, `readline`, `module` (plus the `@mariozechner/*` aliases).
- `pi` ExtensionAPI: `on`, `registerTool`, `registerCommand`, `registerShortcut`, `registerFlag`,
  `getFlag`, `registerMessageRenderer`, `registerEntryRenderer`, `registerMarkdownTransformer`,
  `sendMessage`, `sendUserMessage`, `appendEntry`, `setSessionName`, `getSessionName`, `setLabel`,
  `exec`, `getActiveTools`, `getAllTools`, `setActiveTools`, `getCommands`, `setModel`,
  `getThinkingLevel`, `setThinkingLevel`, `registerProvider`, `unregisterProvider`, `events`.
- `ctx`: `ui` (select, confirm, input, editor, notify, setStatus, setWidget, setTitle,
  setWorkingMessage, setEditorText, getEditorText, theme), `sessionManager`, `modelRegistry`
  (find, getAvailable, complete), `model`, `thinkingLevel`, `signal`, `isIdle`, `abort`,
  `hasPendingMessages`, `shutdown`, `getContextUsage`, `getSystemPrompt`; command context adds
  `waitForIdle` and `getSystemPromptOptions`.
- Event dispatch with pi's semantics: `tool_call` first block wins and input mutation,
  `tool_result` patch chaining, `input` transform/handled chaining, `message_end` replacement,
  `before_agent_start` message injection and system prompt chaining, `context` replacement,
  `before_provider_request` payload replacement, `before_provider_headers`, `after_provider_response`,
  `session_before_*` cancel, `project_trust`, `resources_discover` aggregation, `user_bash`.
- Node-like globals: `console`, timers, `AbortController`, `fetch` (via reqwest), `process`
  (live `env`, `cwd`, `platform`), `crypto.randomUUID`, `TextEncoder/Decoder`, `btoa/atob`,
  `structuredClone`.
- Errors in handlers are reported to the host, never fatal; cancellation reaches handlers as an
  `AbortSignal`.
- Extension-declared `renderCall`/`renderResult`/message renderers are invoked; their plain-text
  output is displayed.

### pi-cli (`pirs`)
- Built-in tools `read`, `bash`, `edit`, `write`, `grep`, `find`, `ls` with pi's names,
  descriptions, schemas, prompt snippets and guidelines, truncation limits (2000 lines / 50 KB),
  output formats, and `details` shapes. Bash runs in a process group, streams partial output,
  honours timeout and abort, spills full output to a temp file when truncated. Grep/find are
  in-process (`ignore` + `regex` + `globset`) and respect `.gitignore`.
- Session manager: pi's JSONL v3 format and file layout, lazy file creation, tree entries
  (`message`, `model_change`, `thinking_level_change`, `compaction`, `custom`, `custom_message`,
  `label`, `session_info`, `branch_summary`), branching, fork, listing, v1/v2 migration,
  unknown entries preserved.
- Settings from `~/.pi/agent/settings.json` and `.pi/settings.json`; `models.json` from both.
- Extension discovery from `~/.pi/agent/extensions`, `.pi/extensions`, settings `extensions`,
  and `-e` flags.
- System prompt builder ported from `system-prompt.ts` (preamble, tools, rules, docs, addendum,
  project context, cwd, custom sections); AGENTS.md / CLAUDE.md discovery from root to cwd plus
  the global file. The `docs` section points the model at `README.md`, `docs/`, `examples/`
  and `STATUS.md` (resolved from `PIRS_DOCS_DIR`, the source tree, or `~/.pi/agent/pirs`) so
  it can write extensions for itself; verified by having pirs build and test one.
- Documentation: `docs/extensions.md` (pirs API as implemented), pi's extension and session
  format references, and `examples/extensions/` with seven working pi examples.
- `AgentSession`: input handling (extension commands, `!`/`!!` shell, `input` event,
  `before_agent_start`), persistence of every message, extension dispatch of every lifecycle
  event, tool-call interception, provider header/payload/response hooks, model and thinking
  switching, `sendMessage` delivery modes (steer, followUp, nextTurn, triggerTurn), shutdown.
- Modes: print (`-p`, final text), JSON (`--mode json`, event stream), interactive TUI
  (inline viewport with streaming tail, extension widgets and status entries, dialogs for
  extension `select`/`confirm`/`input` with timeouts, history, steering while running,
  `/help /model /thinking /tools /extensions /session /new /reload /clear /exit`).
- Live extension reload (`/reload`, `ctx.reload()`): `session_shutdown(reload)` to the old
  runtime, then the QuickJS host thread is replaced by a fresh one (so changed modules and
  transitive imports are re-evaluated and stale timers die), extension sources are
  re-discovered (new/removed files picked up), context files re-read, tools and commands
  re-registered, then `session_start(reload)` and `resources_discover(reload)`. Refused while
  the agent is running. `ctx.reload()` hands the work to the main runtime because it is called
  from the very thread being torn down.
- CLI flags: `-p`, `--mode`, `-m/--model`, `--thinking`, `-e`, `--no-extensions`, `-c`,
  `-r`, `--no-session`, `--session-dir`, `--system-prompt`, `--append-system-prompt`, `--tools`,
  `--sequential-tools`, `--cwd`, `--list-models`, `--list-extensions`.

## Not implemented

### Extension surface (by design of this port)
- Custom TUI components: `ctx.ui.custom()`, custom editors, footers, headers, overlays,
  autocomplete providers, themes, `onTerminalInput`. Theme colour functions return plain text.
- CommonJS npm packages (`require`, `module.exports`), Node streams, `node:zlib`, `spawn`.
- Custom `streamSimple` provider implementations from extensions (declarative providers work).
- `ctx.newSession`, `ctx.fork`, `ctx.navigateTree`, `ctx.switchSession`
  return `{ cancelled: true }`.
- `ctx.modelRegistry.streamSimple` returns the completed message only (no token stream).
- `pi.registerShortcut` handlers are stored but the TUI does not yet bind keys to them.
- Flag values from the command line (`--<flag>`) are not parsed; `getFlag` returns defaults.
- `ctx.isProjectTrusted()` always returns true; there is no project trust prompt.

### Agent and CLI features
- Compaction (manual and automatic) and context-overflow recovery.
- `/tree`, `/fork`, `/resume` navigation in the TUI (the session manager supports branching and
  forking, the UI does not expose it).
- Skills, prompt templates, `/skill:` and template expansion.
- RPC mode, package installation (`pi install`, `packages` setting), interactive OAuth login
  flows (`/login`; existing pi or Claude Code logins are reused), update checks.
- Image attachments from the editor and image resizing (the `read` tool does return images).
- Markdown rendering in the TUI (plain wrapped text), tool output expansion, multi-line editor
  features beyond alt+enter, autocomplete.
- Google, Bedrock, Mistral, OpenAI Responses and other pi provider APIs; only
  `anthropic-messages` and `openai-completions` exist.
- Model catalog costs for Claude 5 models are best-effort placeholders; override them in
  `models.json` if exact accounting matters.
- Windows support (the bash tool and paths assume a Unix shell).

## Known issues and notes
- The TUI must never query the terminal cursor position while crossterm's `EventStream` is
  alive: the query blocks for two seconds. `TrackedBackend` in `modes/interactive.rs` exists for
  this; keep using it.
- `PIRS_TRACE=1` prints timestamped dispatch traces to stderr for debugging latency.
- `cargo clippy` reports a handful of style warnings (large enum variants, collapsible matches);
  none affect behaviour.
- A reference checkout of pi is expected at the path hard-coded in the pi-ext tests and the
  `sweep` example; those tests skip when it is absent.

## Suggested next steps
A redesign is under discussion in `docs/design/` (start at `00-north-star.md`) (loop server + protocol + DSL,
components behind protocols); if adopted, its phase 0 supersedes this list. See `HANDOFF.md`.

1. Live-test the OpenAI provider with a real key; try thinking levels and tool-heavy sessions
   against Anthropic.
2. Compaction, then `/tree` and `/fork` in the TUI.
3. Shortcut key binding and CLI flag parsing for extensions.
4. Markdown rendering and tool output expansion in the TUI.
5. Skills and prompt templates.
