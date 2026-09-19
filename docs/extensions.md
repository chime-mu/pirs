# Writing pirs extensions

This document is the contract for anyone (usually a model) turning a description of desired
behaviour into a working pirs extension. It is the complete list of what pirs supports. Do not
use anything not listed here; pi's full reference in
[pi-extensions-reference.md](pi-extensions-reference.md) describes pi, and where the two
disagree this file wins.

An extension is a TypeScript module exporting a default function that receives the `pi`
ExtensionAPI. It can register tools the model can call, add slash commands, intercept and
block tool calls, transform user input, inject context, prompt the user, show status, and
persist state in the session.

## Procedure

Given a description of what the extension should do:

1. Map each sentence of the description to one of the [Recipes](#recipes) or, failing that, to
   an event in [Lifecycle](#lifecycle) or a method in [`pi`](#pi-extensionapi). If a sentence
   needs something in [Limitations](#limitations), say so and propose the nearest supported
   alternative instead of writing code that silently no-ops.
2. Write one file. Keep it as short as the description; do not add configuration, options, or
   abstractions the description does not ask for.
3. Follow every item in [Rules](#rules).
4. Run `pirs --list-extensions -e <file>`. It must load without error and list exactly the
   tools and commands the description implies. Fix and re-run until it does.
5. Report the file path, what `--list-extensions` printed, and any part of the description that
   could not be implemented.

## Rules

- Use only the events, `ctx` members, `pi` methods, and imports listed in this file.
- Check `ctx.hasUI` before calling any `ctx.ui` dialog (`select`, `confirm`, `input`). In print
  mode dialogs resolve to `undefined`/`false` immediately; the extension must still behave
  sensibly.
- Run programs with `pi.exec(cmd, args, { cwd: ctx.cwd, signal: ctx.signal })`. Never
  `child_process.spawn`; it throws.
- Pass `ctx.signal` to `fetch` and `pi.exec` so Esc aborts them.
- Persist state in `pi.appendEntry` entries or tool result `details`, and rebuild it in
  `session_start` from `ctx.sessionManager.getBranch()` / `getEntries()`. Module-level variables
  are lost on `/reload` and restart.
- Throw an `Error` from tool `execute` to report failure; do not return error text as success.
- Validate nothing that `parameters` already validates. Do validate anything else before acting.
- Blocking in `tool_call` is final for that call: the first handler that returns
  `{ block: true }` wins. Order-sensitive logic between extensions must be stated in the
  description and implemented by load order (file name or `-e` order), not assumed.
- `input` transforms chain; each handler sees the previous handler's text. Return nothing when the
  input is not yours.
- `before_agent_start` system prompt changes are per run; `context` changes are per request.
  Neither is persisted. Use `pi.sendMessage` for something the model should remember.
- After `await ctx.reload()` nothing else runs on this runtime. Make it the last statement.
- Colours and TUI components render as plain text. Do not depend on layout, colour, or
  keyboard bindings.
- Name tools with lowercase letters, digits, and underscores. Give every tool a
  `promptSnippet` and, when the model needs guidance on when to use it, `promptGuidelines`
  that name the tool.

## Quick start

Create `~/.pi/agent/extensions/my-extension.ts` (loaded for every project) or
`.pi/extensions/my-extension.ts` (this project only), or pass a file with `pirs -e ./my-extension.ts`.

```typescript
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export default function (pi: ExtensionAPI) {
  pi.on("session_start", async (_event, ctx) => {
    ctx.ui.notify("Extension loaded!", "info");
  });

  pi.on("tool_call", async (event, ctx) => {
    if (event.toolName === "bash" && String(event.input.command).includes("rm -rf")) {
      const ok = await ctx.ui.confirm("Dangerous!", "Allow rm -rf?");
      if (!ok) return { block: true, reason: "Blocked by user" };
    }
  });

  pi.registerTool({
    name: "greet",
    label: "Greet",
    description: "Greet someone by name",
    parameters: Type.Object({ name: Type.String({ description: "Name to greet" }) }),
    async execute(_toolCallId, params) {
      return { content: [{ type: "text", text: `Hello, ${params.name}!` }], details: {} };
    },
  });

  pi.registerCommand("hello", {
    description: "Say hello",
    handler: async (args, ctx) => ctx.ui.notify(`Hello ${args || "world"}!`, "info"),
  });
}
```

Check what an extension registers without starting a session:

```
pirs --list-extensions -e ./my-extension.ts
```

Extensions in `examples/extensions/` (copied from pi) are known to work: `hello.ts`,
`permission-gate.ts`, `protected-paths.ts`, `dynamic-tools.ts`, `todo.ts`, `git-checkpoint.ts`,
`input-transform.ts`. `fetch.ts` is a pirs-written tool of realistic size.

## Recipes

Each recipe is the complete idiom for one kind of intent. Combine as many as the description
needs inside one default export.

**Block or rewrite a tool call** ("refuse X", "ask before Y", "always add flag Z")

```typescript
pi.on("tool_call", async (event, ctx) => {
  if (event.toolName !== "bash") return;
  const cmd = String(event.input.command ?? "");
  if (/\brm\s+-rf\b/.test(cmd)) {
    if (!ctx.hasUI) return { block: true, reason: "rm -rf is not allowed in print mode" };
    const ok = await ctx.ui.confirm("Destructive command", cmd);
    if (!ok) return { block: true, reason: "Blocked by user" };
  }
  // rewrite: event.input.command = cmd + " --dry-run";
});
```

For path-based rules (`edit`, `write`, `read`) check `String(event.input.path ?? "")` resolved
against `ctx.cwd`.

**Transform or consume user input** ("expand `!{cmd}`", "treat lines starting with ? as ...")

```typescript
pi.on("input", async (event, ctx) => {
  if (!event.text.startsWith("?")) return;                 // not ours
  return { action: "transform", text: "Explain briefly: " + event.text.slice(1) };
  // or: return { action: "handled" };                     // consumed, no model call
});
```

**Add to the system prompt or inject context** ("tell the model about X on every run")

```typescript
pi.on("before_agent_start", async (event, ctx) => {
  const rules = readRules(ctx.cwd);                          // your own helper
  if (!rules) return;
  return { systemPrompt: event.systemPrompt + "\n\n" + rules };
});
```

To show the user what was injected as well, return
`{ message: { customType: "my-ext", content: rules, display: true } }` instead.

**Show something in the status line** ("show the git branch", "show a counter")

```typescript
async function refresh(ctx) {
  const r = await pi.exec("git", ["branch", "--show-current"], { cwd: ctx.cwd });
  ctx.ui.setStatus("branch", r.code === 0 ? r.stdout.trim() : undefined);
}
pi.on("session_start", (_e, ctx) => refresh(ctx));
pi.on("agent_end", (_e, ctx) => refresh(ctx));
```

Use `ctx.ui.setWidget(key, lines)` for more than one line.

**Add a slash command** ("`/foo args` does ...")

```typescript
pi.registerCommand("foo", {
  description: "What /foo does",
  handler: async (args, ctx) => {
    await ctx.waitForIdle();
    ctx.ui.notify(`foo ${args}`, "info");
  },
});
```

**Give the model a new tool** — see [Custom tools](#custom-tools).

**Remember something across turns and restarts** ("track todos", "remember decisions")

```typescript
let items: string[] = [];
pi.on("session_start", (_e, ctx) => {
  items = [];
  for (const e of ctx.sessionManager.getBranch()) {
    if (e.type === "custom" && e.customType === "my-ext:item") items.push(e.data.text);
  }
});
function add(text: string) { items.push(text); pi.appendEntry("my-ext:item", { text }); }
```

**Do something when a run finishes** ("commit after each turn", "notify when done")

```typescript
pi.on("agent_end", async (_event, ctx) => {
  await pi.exec("git", ["add", "-A"], { cwd: ctx.cwd });
  await pi.exec("git", ["commit", "-qm", "pirs checkpoint"], { cwd: ctx.cwd });
});
```

**Send the model a message from outside** ("when file X changes, tell the model")

```typescript
pi.sendMessage(
  { customType: "my-ext", content: "File X changed:\n" + contents, display: true },
  { deliverAs: "followUp", triggerTurn: true },
);
```

## Composing descriptions

Several descriptions can be implemented as one file or as several; behaviour is the same because
all handlers for an event run in load order. When combining:

- Independent intents (a tool plus a status line, a command plus a guard) simply coexist. Put
  them in one default export or in separate files; nothing else is needed.
- Two intents on the same event must be checked for interaction. `tool_call`: the first block
  wins, so put the stricter rule first. `input`: transforms chain, so order determines what
  the second one sees. `before_agent_start`: later handlers see earlier `systemPrompt`
  edits. If the descriptions do not say which comes first, ask or pick the safer order and
  state it in the report.
- Two intents that both want `setStatus` must use different keys. Two that want the same
  tool name conflict; the later registration replaces the earlier one.
- Shared state between files goes through `pi.events` (in-process) or `pi.appendEntry`
  (persisted), never through module imports of each other's internals.

## Locations and loading

| Location | Scope |
|----------|-------|
| `~/.pi/agent/extensions/*.ts` | global, one file per extension |
| `~/.pi/agent/extensions/<dir>/index.ts` | global, directory extension |
| `.pi/extensions/*.ts` and `.pi/extensions/<dir>/index.ts` | project |
| `"extensions": [...]` in `settings.json` | files or directories |
| `pirs -e <file or dir>` | one run; a directory loads its `index.ts` |

A directory may carry a `package.json` with `"pi": { "extensions": ["./src/index.ts"] }`.
`--no-extensions` skips auto-discovery (explicit `-e` flags still load).

Files are TypeScript with erasable syntax (type annotations, interfaces, `import type`).
Enums and parameter properties are also handled through a transform fallback. The default
export may be `async`; pirs awaits it before `session_start`.

## Available imports

| Import | Provides |
|--------|----------|
| `typebox` (also `@sinclair/typebox`) | `Type.Object/String/Number/Integer/Boolean/Array/Optional/Union/Literal/Record/Any/Unknown`, `StringEnum`, `Value.Check` |
| `@earendil-works/pi-ai` | `Type`, `StringEnum`, `uuidv7`, `complete(model, context, options)`, `getModels` |
| `@earendil-works/pi-coding-agent` | `defineTool`, `isToolCallEventType`, `isBashToolResult` (and `isReadToolResult`, ...), `CONFIG_DIR_NAME`, `createBashTool`/`createReadTool`/`createEditTool`/`createWriteTool`/`createGrepTool`/`createFindTool`/`createLsTool`, `truncateHead`, `truncateTail`, `parseFrontmatter`, `serializeConversation`, `SessionManager.list`, `keyHint` |
| `@earendil-works/pi-tui` | `Text`, `Container`, `Box`, `Spacer`, `matchesKey`, `truncateToWidth`, `fuzzyFilter`, `Key` (plain-text components only) |
| `node:fs`, `node:fs/promises` | `existsSync`, `readFileSync`, `writeFileSync`, `appendFileSync`, `mkdirSync`, `readdirSync`, `statSync`, `unlinkSync`, `rmSync`, `renameSync`, `copyFileSync`, `realpathSync`, `mkdtempSync`, `promises.*` |
| `node:path` | `join`, `resolve`, `dirname`, `basename`, `extname`, `relative`, `normalize`, `isAbsolute`, `parse`, `sep` |
| `node:os` | `homedir`, `tmpdir`, `platform`, `hostname`, `arch`, `EOL` |
| `node:child_process` | `execSync`, `execFileSync`, `spawnSync`, `exec` (callback). Prefer `pi.exec`. |
| `node:util`, `node:url`, `node:crypto`, `node:events`, `node:process`, `node:readline`, `node:module` | common helpers (`promisify`, `fileURLToPath`, `randomUUID`, `EventEmitter`) |
| `./relative.ts` | other files next to the extension |
| `some-package` | an ESM package installed in a `node_modules` folder next to the extension (CommonJS packages do not work) |

Globals: `console`, `setTimeout`/`setInterval`, `fetch` (returns a Response with `text()` and
`json()`), `AbortController`, `process.env` (live), `process.cwd()`, `crypto.randomUUID`,
`TextEncoder`/`TextDecoder`, `btoa`/`atob`, `structuredClone`.

## Events

Subscribe with `pi.on(name, async (event, ctx) => { ... })`. Handlers run in extension load
order, then registration order. A handler that throws is reported (shown in the UI and in
`/extensions`) and skipped; it never crashes pirs. The unsubscribe function returned by `pi.on`
removes that handler.

### Lifecycle

```
startup
  session_start { reason: "startup" }
  resources_discover { cwd, reason: "startup" }     (return value is collected but unused)
user submits input
  (extension command? -> its handler runs, nothing else)
  input { text, images?, source }                   -> continue | transform | handled
  before_agent_start { prompt, systemPrompt, systemPromptOptions }
  agent_start
  turn_start
    message_start / message_update* / message_end   (assistant streaming)
    tool_execution_start
    tool_call { toolName, toolCallId, input }       -> { block, reason, terminate }
    tool_execution_update*
    tool_result { toolName, toolCallId, input, content, details, isError }
    tool_execution_end
    message_start / message_end                     (tool result message)
  turn_end
  ... more turns while the model calls tools ...
  agent_end { messages }
  agent_settled
each provider request
  context { messages }                              -> { messages }
  before_provider_headers { headers }               (mutate in place)
  before_provider_request { payload }               -> replacement payload
  after_provider_response { status, headers }
changes
  model_select { model, previousModel, source }
  thinking_level_select { level, previousLevel }
  session_info_changed { name }
/reload (or ctx.reload() from a command)
  session_shutdown { reason: "reload" }             to the old runtime, which is then discarded
  session_start { reason: "reload" }                to a fresh runtime with re-read extension files
  resources_discover { cwd, reason: "reload" }
exit
  session_shutdown { reason: "quit" }
```

### Result semantics

- `tool_call`: return `{ block: true, reason }` to stop the tool; the model receives `reason`
  as an error result. `terminate: true` asks the agent to stop after this batch. You may mutate
  `event.input` to rewrite arguments. The first blocking handler wins.
- `tool_result`: return any of `content`, `details`, `isError`, `usage` to patch the result;
  later handlers see the patched values.
- `input`: return `{ action: "transform", text }` to rewrite the prompt, `{ action: "handled" }`
  to consume it (no model call), or nothing to continue. Transforms chain.
- `before_agent_start`: return `{ message: { customType, content, display } }` to inject a
  custom message into this run, and/or `{ systemPrompt }` to replace the system prompt for this
  run. `event.systemPrompt` reflects earlier handlers' changes.
- `context`: return `{ messages }` (a filtered or rewritten copy) to change what the model sees
  for this request only. Nothing is persisted.
- `message_end`: for assistant messages, return `{ message }` with the same role to replace the
  message before it is persisted.
- `before_provider_request`: return a value to replace the provider payload.
- `before_provider_headers`: set `event.headers[name] = value` (or `null` to delete).
- `session_before_switch`, `session_before_fork`, `session_before_compact`,
  `session_before_tree`, `project_trust`, `user_bash`, `session_tree`, `session_compact`:
  accepted for compatibility, but pirs never fires them (no compaction, tree navigation, session
  switching, or trust prompts yet).

## `ctx` (ExtensionContext)

All handlers, tool `execute` functions, and command handlers receive `ctx`.

| Member | Notes |
|--------|-------|
| `ctx.ui.select(title, options[], opts?)` | picker; resolves to the chosen string or `undefined` |
| `ctx.ui.confirm(title, message, opts?)` | Yes/No; `false` when cancelled |
| `ctx.ui.input(title, placeholder?, opts?)` | single-line input or `undefined` |
| `ctx.ui.notify(message, "info" \| "warning" \| "error")` | one-line notice in the transcript |
| `ctx.ui.setStatus(key, text \| undefined)` | text in the status line |
| `ctx.ui.setWidget(key, lines[] \| undefined, { placement })` | lines above (default) or below the editor |
| `ctx.ui.setTitle`, `setWorkingMessage`, `setEditorText`, `getEditorText` | supported |
| `ctx.ui.theme.fg(color, text)` | returns `text` unchanged (plain-text theme) |
| `ctx.ui.custom`, `editor`, `setFooter`, `setHeader`, `setEditorComponent`, `addAutocompleteProvider`, `onTerminalInput` | not supported; `custom` throws, the rest are no-ops |
| `ctx.hasUI`, `ctx.mode` | `true`/`"tui"` interactively, `false`/`"print"` or `"json"` in print mode. Always check `hasUI` before prompting; in print mode dialogs resolve to `undefined`/`false` immediately. |
| `ctx.cwd` | working directory |
| `ctx.sessionManager.getEntries()`, `getBranch()`, `getLeafId()`, `getSessionFile()`, `getSessionId()`, `getSessionName()`, `getLabel(id)` | read the session (see pi's session format: entries with `type`, `id`, `parentId`, `message`) |
| `ctx.modelRegistry.getAvailable()`, `getAll()`, `find(spec)`, `complete(model, context, options)` | model catalog and a non-streaming completion helper |
| `ctx.model`, `ctx.thinkingLevel` | current model and level |
| `ctx.signal` | `AbortSignal` that fires when the user aborts the run (Esc); pass it to `fetch` and `pi.exec` |
| `ctx.isIdle()`, `ctx.abort()`, `ctx.hasPendingMessages()`, `ctx.shutdown()` | control |
| `ctx.getContextUsage()`, `ctx.getSystemPrompt()`, `ctx.compact()` | usage `{ tokens, contextWindow, percent }`; `compact` is a no-op |

Command handlers additionally get `ctx.waitForIdle()`, `ctx.getSystemPromptOptions()` and
`ctx.reload()`. `ctx.reload()` does the same as `/reload`: the whole extension runtime is torn down
and rebuilt (extension files, including new or deleted ones in `.pi/extensions/` and
`~/.pi/agent/extensions/`, are re-read from disk; `AGENTS.md`-style context files are re-read too).
Treat it as terminal for the calling handler: it returns immediately, and any code after
`await ctx.reload()` runs on the old runtime, which goes away moments later. Reload is refused
while the agent is running.
`ctx.newSession`, `fork`, `navigateTree`, `switchSession` resolve to `{ cancelled: true }`.

## `pi` (ExtensionAPI)

| Method | Notes |
|--------|-------|
| `pi.on(event, handler)` | returns an unsubscribe function |
| `pi.registerTool(def)` | see below; works at load time and later (tools refresh immediately) |
| `pi.registerCommand(name, { description, handler(args, ctx), getArgumentCompletions? })` | `/name args` in the TUI |
| `pi.registerShortcut(key, { handler })` | stored; the TUI does not bind keys yet |
| `pi.registerFlag(name, { type, default })`, `pi.getFlag(name)` | defaults only; no CLI parsing yet |
| `pi.sendMessage({ customType, content, display, details }, { deliverAs, triggerTurn })` | `deliverAs`: `"steer"` (default, delivered after the current tool batch), `"followUp"` (after the agent finishes), `"nextTurn"` (with the next user prompt). When idle, `triggerTurn: true` starts a run. |
| `pi.sendUserMessage(text \| blocks, { deliverAs })` | a real user message; starts a run when idle |
| `pi.appendEntry(customType, data)` | persist extension data in the session (not sent to the model); read back with `ctx.sessionManager.getEntries()` filtering `type === "custom"` |
| `pi.setSessionName`, `getSessionName`, `setLabel(entryId, label)` | session metadata |
| `pi.exec(command, args[], { cwd, timeout, signal })` | run a program; resolves `{ stdout, stderr, code, killed }` |
| `pi.getActiveTools()`, `getAllTools()`, `setActiveTools(names[])` | tool selection; extension tools are active by default |
| `pi.getCommands()` | `{ name, description, source }[]` |
| `pi.setModel(model)`, `getThinkingLevel()`, `setThinkingLevel(level)` | model control; `model` objects come from `ctx.modelRegistry` |
| `pi.registerProvider(name, { baseUrl, apiKey, api, models })` | add an OpenAI-compatible or Anthropic-compatible provider (`api`: `"openai-completions"` or `"anthropic-messages"`); custom `streamSimple` functions are ignored |
| `pi.events.on/emit/off` | in-process event bus between extensions |
| `pi.registerMessageRenderer(customType, fn)`, `registerEntryRenderer` | called; the component's `render(width)` lines are shown as plain text |

## Custom tools

```typescript
pi.registerTool({
  name: "my_tool",                         // lowercase, letters/digits/underscore
  label: "My Tool",
  description: "What the tool does (shown to the model)",
  promptSnippet: "One line for the tools list in the system prompt",
  promptGuidelines: ["Use my_tool when ..."],   // name the tool explicitly
  parameters: Type.Object({
    action: StringEnum(["list", "add"] as const),
    text: Type.Optional(Type.String({ description: "..." })),
  }),
  executionMode: "sequential",             // optional; default runs in parallel with other calls
  prepareArguments(args) { return args; },  // optional compatibility shim before validation
  async execute(toolCallId, params, signal, onUpdate, ctx) {
    onUpdate?.({ content: [{ type: "text", text: "working..." }] });   // streamed progress
    if (signal?.aborted) throw new Error("aborted");
    return {
      content: [{ type: "text", text: "result for the model" }],    // text and { type: "image", data, mimeType }
      details: { anything: "for logs and UI, persisted in the session" },
    };
  },
  renderCall(args, theme) { return new Text(`my_tool ${args.action}`); },      // optional, plain text
  renderResult(result, { expanded }, theme) { return new Text("done"); },     // optional
});
```

Throw an `Error` to report a failure; the message becomes an error tool result. Arguments are
validated against `parameters` (required keys and primitive types) before `execute` runs.
Registering a tool with the name of a built-in (`read`, `bash`, `edit`, `write`, `grep`, `find`,
`ls`) overrides it; `createBashTool(cwd)` and friends from `@earendil-works/pi-coding-agent`
return the built-in implementation so a wrapper can delegate to it.

State that must survive restarts belongs in tool result `details` (rebuild it in `session_start`
from `ctx.sessionManager.getBranch()`) or in `pi.appendEntry` entries, exactly as in pi's `todo.ts`.

## Limitations

- No custom TUI components (`ctx.ui.custom`, editors, overlays, headers, footers, themes,
  autocomplete). Use `select`, `confirm`, `input`, `notify`, `setStatus`, `setWidget`.
- Colours: `theme.fg`, `theme.bold`, etc. return plain text.
- CommonJS packages and Node streams are unavailable; `child_process.spawn` throws (use `pi.exec`).
- Compaction, tree navigation, session switching and forking are not implemented, so
  their events never fire and their `ctx` methods return `{ cancelled: true }`.
- Shortcuts and flags are registered but not yet wired to keys or CLI arguments.
- Custom provider stream implementations are not supported; declarative providers are.

## Debugging

- `pirs --list-extensions -e ./ext.ts` loads the extension and prints what it registered, or
  the load error (syntax errors, unresolved imports).
- `/extensions` in the TUI lists commands and every handler error with its stack.
- `console.log` output appears as dim lines in the transcript (stderr in print mode).
- `PIRS_TRACE=1` prints dispatch timings to stderr.
