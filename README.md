# pirs

A Rust port of [pi](https://github.com/earendil-works/pi), the self-extensible coding agent.
The binary is `pirs`. It reads the same config directory as pi (`~/.pi/agent`), writes
pi-compatible session files, and runs pi's TypeScript extensions unchanged.

```
crates/
  pi-ai      unified LLM API: message/content types, Anthropic Messages and OpenAI
             Chat Completions streaming providers, model catalog, a scripted "faux" provider
  pi-agent   agent runtime: AgentTool trait, event protocol, agent loop with sequential and
             parallel tool execution, before/after tool-call hooks, steering and follow-up queues
  pi-ext     extension host: embedded QuickJS runtime, TypeScript type stripping (swc), the
             `pi` ExtensionAPI, event dispatch with pi's chaining semantics, virtual modules
  pi-cli     the `pirs` binary: built-in tools, session manager, settings, system prompt,
             print/JSON modes, interactive TUI
```

## Build and run

```bash
cargo build --release
export ANTHROPIC_API_KEY=...          # or OPENAI_API_KEY, or ~/.pi/agent/models.json
./target/release/pirs                 # interactive
./target/release/pirs -p "summarize this repo"      # print mode
./target/release/pirs -p --mode json "..."          # JSON event stream
./target/release/pirs -e ./my-extension.ts          # load an extension
./target/release/pirs --list-models
./target/release/pirs --list-extensions -e ~/.pi/agent/extensions
./target/release/pirs -c                            # continue the latest session in this cwd
```

Offline demo without API keys: the `faux` provider replays a JSON script.

```bash
echo '[{"text":"Looking.","toolCalls":[{"name":"bash","arguments":{"command":"ls"}}]},"Done."]' > script.json
PIRS_FAUX_SCRIPT=$PWD/script.json pirs -p --model faux/scripted "hi"
```

`cargo test --workspace` runs the unit and integration tests (the extension host tests load
pi's own example extensions when a pi checkout is present at the path in `crates/pi-ext/src/lib.rs`).

## The extension model

pi extensions are TypeScript modules with a default export `function (pi: ExtensionAPI)`.
pirs runs them inside an embedded QuickJS engine on a dedicated thread:

1. `swc_ts_fast_strip` removes type annotations (with a full transform as fallback for
   enums and other non-erasable syntax).
2. A custom module resolver serves `typebox`, `@earendil-works/pi-ai`,
   `@earendil-works/pi-coding-agent`, `@earendil-works/pi-agent-core`, `@earendil-works/pi-tui`
   and `node:fs/path/os/child_process/util/url/crypto/process/events/readline/module` from
   embedded shims. Relative imports and ESM packages in a `node_modules` folder resolve normally.
3. `crates/pi-ext/src/js/runtime.js` implements the `pi` API (`on`, `registerTool`,
   `registerCommand`, `registerShortcut`, `registerFlag`, `sendMessage`, `sendUserMessage`,
   `appendEntry`, `exec`, `setModel`, `setActiveTools`, `registerProvider`, `events`, ...),
   the `ctx` object (`ui.select/confirm/input/notify/setStatus/setWidget`, `sessionManager`,
   `modelRegistry`, `signal`, `abort`, `shutdown`, ...), and pi's per-event result semantics
   (`tool_call` first-block-wins, `tool_result` patch chaining, `input` transform chaining,
   `message_end` replacement, `before_agent_start` prompt chaining, `context` replacement, ...).
4. Everything that touches the outside world goes through two native functions
   (`__hostSync`, `__hostAsync`) that dispatch to the `HostCallbacks` trait implemented by the
   application. Async handlers can await host UI dialogs; cancellation reaches JS as an
   `AbortSignal`.

Result of loading pi's 77 example extensions unchanged (`cargo run -p pi-ext --example sweep -- <dir>`):
71 load and register their tools, commands, and handlers. The 6 that fail need npm packages
that are not installed in the checkout (`@anthropic-ai/sdk`, `ms`, `@earendil-works/gondolin`,
`@anthropic-ai/sandbox-runtime`) or Node streams (`fs.createReadStream`, `node:zlib`).

Verified end to end against the agent loop: `hello.ts` (custom tool), `permission-gate.ts`
(async `tool_call` handler awaiting `ctx.ui.select`, blocks or allows a command), `protected-paths.ts`
(blocks writes), `dynamic-tools.ts` (tools registered after startup), `todo.ts` (stateful tool),
and pi-style `input`, `tool_result`, `before_agent_start` handlers.

### Not supported (by design of this port)

- Custom TUI components: `ctx.ui.custom()`, custom editors, footers, headers, overlays. Tool
  `renderCall`/`renderResult` and message renderers are called and their plain-text output shown;
  theme colour functions return plain text.
- CommonJS npm packages (`require`, `module.exports`) and Node streams. Node built-ins are shims
  covering the common synchronous file/path/os/child_process/crypto APIs plus `fetch`, timers,
  `AbortController`, `TextEncoder/Decoder`, `process.env`.
- Custom `streamSimple` providers registered from extensions (`registerProvider` with `api`,
  `baseUrl`, `apiKey`, and `models` works).
- `ctx.newSession`, `ctx.fork`, `ctx.navigateTree`, `ctx.switchSession`, `ctx.reload` return
  `{ cancelled: true }`.

## What else is and is not ported

Ported: Anthropic and OpenAI-compatible streaming with thinking/reasoning, tool calls, usage and
cost; parallel and sequential tool execution; the seven built-in tools (`read`, `bash`, `edit`,
`write`, `grep`, `find`, `ls`) with pi's schemas, truncation limits and output formats; session
JSONL v3 files (tree entries, model/thinking changes, custom entries, labels, names) that pi can
open; AGENTS.md/CLAUDE.md project context; `~/.pi/agent/settings.json` and `models.json`;
`!`/`!!` shell commands; steering messages while the agent runs; print, JSON and interactive modes.

Not ported: compaction, `/tree` and `/fork` navigation, skills and prompt templates, themes,
the RPC mode, package installation (`pi install`), OAuth logins, image resizing, and the
full markdown renderer (the TUI prints plain wrapped text).

## Layout of the interactive mode

The TUI keeps a small inline viewport at the bottom (streaming tail, extension widgets, editor,
status line) and scrolls finished content into the terminal's normal scrollback, like pi.
Keys: enter sends, alt+enter inserts a newline, esc aborts, ctrl+c clears or exits, ctrl+d exits,
up/down browse history. Slash commands: `/help`, `/model`, `/thinking`, `/tools`, `/extensions`,
`/session`, `/new`, `/clear`, `/exit`, plus any command registered by an extension.
