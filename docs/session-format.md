# Session File Format

A pirs conversation is one JSONL (JSON Lines) file. Each line is a JSON object with a
`type` field. Entries form a tree through `id`/`parentId`, so a conversation can branch in
place without a second file.

The format is version 3 of pi's session format (MIT), with one addition of pirs's own: a
per-entry `seq`. Everything below describes what pirs writes; pi can still read the message
entries, and pirs reads a file pi wrote. Where a paragraph is about pi's own CLI or
TypeScript API rather than pirs, it says so.

## File Location

```
~/.pirs/sessions/<encoded cwd>/<timestamp>_<conversation-id>.jsonl
```

`PIRS_HOME` moves the whole `~/.pirs` directory, which is how the tests redirect it.
`<conversation-id>` is a time-ordered UUIDv7. `<timestamp>` is the header's ISO timestamp
with `:` and `.` replaced by `-`, so a directory listing sorts by creation time:
`2026-09-21T06-08-36-745Z_01a0c294-a589-7000-8005-cce0dba18dd6.jsonl`. For
`<encoded cwd>`, the leading path separator is removed, `/`, `\` and `:` become `-`, and
the result is wrapped in `--`: `/home/me/proj` is `--home-me-proj--`. That is pi's encoding
unchanged, so the two tools agree on where a directory's conversations live.

The server is the only writer. A client never opens a session file itself: a path in what
the server sends is a label the client displays and hands back, and `fs.read` is how it
reads one (`docs/protocol.md`, "Paths are opaque").

## Deleting Sessions

Delete the `.jsonl` file, and the `refs/` files belonging to it (below). pirs has no
command for this, and no equivalent of pi's interactive deletion from `/resume`.

## Session Version

Sessions have a version field in the header:

- **Version 1**: Linear entry sequence (legacy, auto-migrated on load)
- **Version 2**: Tree structure with `id`/`parentId` linking
- **Version 3**: Renamed `hookMessage` role to `custom` (extensions unification)

Existing sessions are automatically migrated to the current version (v3) when loaded.

## `seq`

Every entry but the header carries a `seq`: an integer that starts at 1 and increases by
one per entry appended to the file. It is assigned on append, and restored on open by
reading the highest `seq` in the file, so the numbering survives a restart and continues
where it left off. A file written by pi has no `seq` at all; those entries are numbered in
file order when the file is read, and the next append continues from the highest number
seen. `seq` is an index, not a count: the numbers of one loop's entries are unique and
increasing, and a reader that skips entries will see gaps.

## The log is the event stream

The session log is not a transcript written beside the events — it *is* them (D-06). Every
sequenced event a loop emits is one entry of its log, and the event's `seq` is that entry's
`seq`. `subscribe { loop, since: n }` replays the logged events with `seq > n` before any
live ones, so a client whose connection dropped catches up on exactly what it missed by
reading the file back. Streaming deltas are the exception: they carry no `seq`, are never
logged and are only ever sent live, so a replay gives the finished message instead of the
fragments that built it.

`crates/pirs-server/src/log.rs` is the conversion, in both directions, and
`docs/protocol.md` has the wire form of each event.

## Source Files

pirs's own implementation: `crates/pirs-server/src/session.rs` (the entry types, the tree,
the file layout) and `crates/pirs-server/src/log.rs` (the log as the event stream).

pi's source on GitHub ([pi](https://github.com/earendil-works/pi)), which is where the type
definitions below come from:
- [`packages/coding-agent/src/core/session-manager.ts`](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/session-manager.ts) - Session entry types and SessionManager
- [`packages/coding-agent/src/core/messages.ts`](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/core/messages.ts) - Extended message types (BashExecutionMessage, CustomMessage, etc.)
- [`packages/ai/src/types.ts`](https://github.com/earendil-works/pi/blob/main/packages/ai/src/types.ts) - Base message types (UserMessage, AssistantMessage, ToolResultMessage)
- [`packages/agent/src/types.ts`](https://github.com/earendil-works/pi/blob/main/packages/agent/src/types.ts) - AgentMessage union type

For TypeScript definitions in your project, inspect `node_modules/@earendil-works/pi-coding-agent/dist/` and `node_modules/@earendil-works/pi-ai/dist/`.

## Message Types

Session entries contain `AgentMessage` objects. These are the types a program that parses a session file has to understand.

### Content Blocks

Messages contain arrays of typed content blocks:

```typescript
interface TextContent {
  type: "text";
  text: string;
  textSignature?: string;
}

interface ImageContent {
  type: "image";
  data: string;      // base64 encoded
  mimeType: string;  // e.g., "image/jpeg", "image/png"
}

interface ThinkingContent {
  type: "thinking";
  thinking: string;
  thinkingSignature?: string;
  redacted?: boolean;
}

interface ToolCall {
  type: "toolCall";
  id: string;
  name: string;
  arguments: Record<string, any>;
  thoughtSignature?: string;
  namespace?: string;
}
```

### Base Message Types (from pi-ai)

```typescript
interface SystemMessage {
  role: "system";
  content: string | TextContent[];
  toolsAdded?: Tool[];
  toolsRemoved?: Array<{ name: string }>;
  timestamp: number;  // Unix ms
}

interface UserMessage {
  role: "user";
  content: string | (TextContent | ImageContent)[];
  timestamp: number;  // Unix ms
}

interface AssistantMessage {
  role: "assistant";
  content: (TextContent | ThinkingContent | ToolCall)[];
  api: string;
  provider: string;
  model: string;
  responseModel?: string;
  responseId?: string;
  providerThinkingLevel?: string;
  diagnostics?: AssistantMessageDiagnostic[];
  usage: Usage;
  stopReason: "pending" | "stop" | "length" | "toolUse" | "error" | "aborted" | "deferred";
  deferred?: DeferredHandle;
  errorMessage?: string;
  rawStopReason?: string;
  endTurn?: boolean;
  timestamp: number;
}

interface ToolResultMessage {
  role: "toolResult";
  toolCallId: string;
  toolName: string;
  content: (TextContent | ImageContent)[];
  details?: any;      // Tool-specific metadata
  usage?: Usage;      // Nested LLM work performed by the tool
  isError: boolean;
  timestamp: number;
}

interface Usage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  cacheWrite1h?: number;
  reasoning?: number;
  totalTokens: number;
  cost: {
    input: number;
    output: number;
    cacheRead: number;
    cacheWrite: number;
    total: number;
  };
}
```

`"pending"` is reserved for partial messages in streaming events. Terminal events replace it with a completion reason before Pi persists the assistant message, so `"pending"` should never appear in session JSONL. `"deferred"` is a terminal reason for a provider response that will complete later; its `deferred` handle contains the provider data needed to retrieve that response.

### Extended Message Types (from pi-coding-agent)

```typescript
interface BashExecutionMessage {
  role: "bashExecution";
  command: string;
  output: string;
  exitCode: number | undefined;
  cancelled: boolean;
  truncated: boolean;
  fullOutputPath?: string;
  excludeFromContext?: boolean;  // true for !! prefix commands
  timestamp: number;
}

interface CustomMessage {
  role: "custom";
  customType: string;            // Extension identifier
  content: string | (TextContent | ImageContent)[];
  display: boolean;              // Show in TUI
  details?: any;                 // Extension-specific metadata
  timestamp: number;
}

interface BranchSummaryMessage {
  role: "branchSummary";
  summary: string;
  fromId: string | null;         // Previous leaf whose abandoned path was summarized
  timestamp: number;
}

interface CompactionSummaryMessage {
  role: "compactionSummary";
  summary: string;
  tokensBefore: number;
  timestamp: number;
}
```

### AgentMessage Union

```typescript
type AgentMessage =
  | SystemMessage
  | UserMessage
  | AssistantMessage
  | ToolResultMessage
  | BashExecutionMessage
  | CustomMessage
  | BranchSummaryMessage
  | CompactionSummaryMessage;
```

## Entry Base

All entries (except `SessionHeader`) extend `SessionEntryBase`:

```typescript
interface SessionEntryBase {
  type: string;
  id: string;           // Usually an 8-char hex ID; may fall back to a full UUID
  parentId: string | null;  // Parent entry ID (null for a root entry)
  timestamp: string;    // ISO timestamp
  seq?: number;         // pirs: 1, 2, 3, … in append order (absent in files pi wrote)
}
```

## Entry Types

### SessionHeader

First line of the file. Metadata only, not part of the tree (no `id`/`parentId`).

```json
{"type":"session","version":3,"id":"uuid","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/path/to/project"}
```

For sessions with a parent (created via `/fork`, `/clone`, or `newSession({ parentSession })`):

```json
{"type":"session","version":3,"id":"uuid","timestamp":"2024-12-03T14:00:00.000Z","cwd":"/path/to/project","parentSession":"/path/to/original/session.jsonl"}
```

### SessionMessageEntry

A message in the conversation. The `message` field contains an `AgentMessage`. System messages carry the prompt and tool loadout: the first request of a session persists one with every prompt section and tool declaration, and later changes persist as system messages that patch `sections` by name (`null` removes one) and list `toolsAdded`/`toolsRemoved`. Replaying them in order yields the current prompt and tools; there is no separate prompt state entry.

```json
{"type":"message","id":"a0b1c2d3","parentId":null,"timestamp":"2024-12-03T14:00:00.000Z","message":{"role":"system","content":"","sections":{"preamble":"You are an expert coding assistant...","tools":"<tools>\n- read: ...\n</tools>","cwd":"/project"},"toolsAdded":[{"name":"read","description":"...","parameters":{}}],"timestamp":1733234400000}}
{"type":"message","id":"d4e5f6g7","parentId":"c3d4e5f6","timestamp":"2024-12-03T14:04:00.000Z","message":{"role":"system","content":"","sections":{"skills":"<skills>...</skills>"},"toolsRemoved":[{"name":"write"}],"timestamp":1733234640000}}
```

Sessions created before system messages existed have no leading system message; the first request declares the current prompt as a later system message, which replays the same way.

```json
{"type":"message","id":"a1b2c3d4","parentId":"prev1234","timestamp":"2024-12-03T14:00:01.000Z","message":{"role":"user","content":"Hello","timestamp":1733234401000}}
{"type":"message","id":"b2c3d4e5","parentId":"a1b2c3d4","timestamp":"2024-12-03T14:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"Hi!"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{...},"stopReason":"stop","timestamp":1733234402000}}
{"type":"message","id":"c3d4e5f6","parentId":"b2c3d4e5","timestamp":"2024-12-03T14:00:03.000Z","message":{"role":"toolResult","toolCallId":"call_123","toolName":"bash","content":[{"type":"text","text":"output"}],"isError":false,"timestamp":1733234403000}}
```

### ModelChangeEntry

Emitted when the user switches models mid-session.

```json
{"type":"model_change","id":"d4e5f6g7","parentId":"c3d4e5f6","timestamp":"2024-12-03T14:05:00.000Z","provider":"openai","modelId":"gpt-4o"}
```

### ThinkingLevelChangeEntry

Emitted when the user changes the thinking/reasoning level.

```json
{"type":"thinking_level_change","id":"e5f6g7h8","parentId":"d4e5f6g7","timestamp":"2024-12-03T14:06:00.000Z","thinkingLevel":"high"}
```

### CompactionEntry

Created when context is compacted. Stores a summary of earlier messages and a complete system prompt/tool checkpoint.

```json
{"type":"compaction","id":"f6g7h8i9","parentId":"e5f6g7h8","timestamp":"2024-12-03T14:10:00.000Z","summary":"User discussed X, Y, Z...","firstKeptEntryId":"c3d4e5f6","tokensBefore":50000,"systemMessage":{"role":"system","content":"You are a coding assistant.","toolsAdded":[],"timestamp":1733235000000}}
```

`firstKeptEntryId` is required. It identifies the first entry retained from before the compaction entry. When rebuilding context, Pi replaces older summarized entries with the compaction summary and keeps the range beginning at this entry.

Optional fields:
- `systemMessage`: The replayed prompt sections and tool declarations at the compaction boundary; it becomes the leading system message of the compacted context, and system messages among the kept entries are dropped in its favor. It is absent on older session entries.
- `usage`: LLM usage from generating the summary; included in session token and cost totals
- `details`: Implementation-specific data (e.g., `{ readFiles: string[], modifiedFiles: string[] }` for default, or custom data for extensions)
- `fromHook`: `true` if generated by an extension, `false`/`undefined` if pi-generated (legacy field name)

### BranchSummaryEntry

Created when switching branches via `/tree` with an LLM generated summary of the left branch up to the common ancestor. Captures context from the abandoned path.

```json
{"type":"branch_summary","id":"g7h8i9j0","parentId":"a1b2c3d4","timestamp":"2024-12-03T14:15:00.000Z","fromId":"f6g7h8i9","summary":"Branch explored approach A..."}
```

`parentId` is the entry from which the new branch continues. `fromId` is the previous leaf whose abandoned path was summarized.

Optional fields:
- `usage`: LLM usage from generating the summary; included in session token and cost totals
- `details`: File tracking data (`{ readFiles: string[], modifiedFiles: string[] }`) for default, or custom data for extensions
- `fromHook`: `true` if generated by an extension, `false`/`undefined` if pi-generated (legacy field name)

### CustomEntry

Extension state persistence. Does NOT participate in LLM context.

```json
{"type":"custom","id":"h8i9j0k1","parentId":"g7h8i9j0","timestamp":"2024-12-03T14:20:00.000Z","seq":17,"customType":"my-extension","data":{"count":42}}
```

Use `customType` to identify your own entries on reload. (In pi, interactive mode can draw
a custom entry with `pi.registerEntryRenderer(customType, renderer)`; pirs has no such
hook — a client reads the entry as an event and draws what it likes.)

### The `pirs.*` custom entries

The loop server writes its own events as `custom` entries under the `pirs.` prefix. They
are not sent to the model, and none of them is a message; each is an event, and its `seq`
is the event's `seq`. The `data` shapes, from `crates/pirs-server/src/log.rs`:

| `customType` | Event | `data` |
|---|---|---|
| `pirs.status` | `loop.status` | `{ state, since, detail? }` — `state` is `working` or `idle`; `since` is Unix ms; `detail` is free text such as `"created"` or `"closed"` |
| `pirs.turn_end` | `loop.turn_end` | `{ messages: [seq, …] }` — the seqs of the turn's message entries, not copies of them |
| `pirs.run_end` | `loop.run_end` | `{ messages: [seq, …] }` — the same for a whole run |
| `pirs.ui.status` | `ui.status` | `{ key, text }` — one status-line key and its current value |
| `pirs.ui.widget` | `ui.widget` | `{ key, lines }` — one widget and its lines |
| `pirs.ui.notify` | `ui.notify` | `{ level, text }` — `level` is `info`, `warning` or `error` |
| `pirs.fs.changed` | `fs.changed` | `{ path, by }` — an absolute path the loop's own `write` or `edit` tool wrote; `by` is `tool` or `turn`. There is no watcher: a file changed by anything else is not reported |
| `pirs.tool_result_rewrite` | — | `{ toolCallId, tool, messageSeq, by, original }` — see below |

`pirs.tool_result_rewrite` is the one with no event. A `[[tool_result]]` policy entry may
rewrite what the model reads back from a tool; the message entry then holds the *rewritten*
result, and this entry follows it holding the original, the handler that changed it (`by`),
and the `seq` of the message it belongs to (`messageSeq`). Nothing reaches the model that
is not in the log beside what it replaced (D-21).

Entries with no event — `model_change`, `thinking_level_change`, `session_info`,
`pirs.tool_result_rewrite` — still consume a `seq`, which is why a replay may skip numbers.

### The `refs/` directory

A tool result whose single text block, or whose base64 image data, is larger than 64 KB
travels **by reference**: the log keeps it in full, because the model needs it, but the
event replaces that block's `text` or `data` with `"[by reference]"` and adds the file to
the message's `details`:

```json
{"ref": {"ref": "<session dir>/refs/<conversation>-<seq>-<block>", "bytes": 70000},
 "refs": [{"index": 0, "ref": "…", "bytes": 70000}]}
```

The files live in `refs/` next to the `.jsonl` files, named
`<conversation-id>-<seq>-<content index>`, and are written once, the first time the event
is produced; a replay reuses them. `details.ref` is the first oversized block, the common
case; `details.refs` lists every one, with `mimeType` for an image, which is written
decoded so the file is the image itself. `fs.read` serves any of them in full.

### CustomMessageEntry

Extension-injected messages that DO participate in LLM context.

```json
{"type":"custom_message","id":"i9j0k1l2","parentId":"h8i9j0k1","timestamp":"2024-12-03T14:25:00.000Z","customType":"my-extension","content":"Injected context...","display":true}
```

Fields:
- `content`: String or `(TextContent | ImageContent)[]` (same as UserMessage)
- `display`: `true` = show in TUI with distinct styling, `false` = hidden
- `details`: Optional extension-specific metadata (not sent to LLM)

### LabelEntry

User-defined bookmark/marker on an entry.

```json
{"type":"label","id":"j0k1l2m3","parentId":"i9j0k1l2","timestamp":"2024-12-03T14:30:00.000Z","targetId":"a1b2c3d4","label":"checkpoint-1"}
```

Set `label` to `undefined` to clear a label.

### SessionInfoEntry

Session metadata: the user-defined display name. In pirs it is set by `pirs --name "…"`,
or by `name` on `loop.create`, and `pirs --list`, `pirs --continue=<name>` and the TUI's
sidebar use it. (pi sets it with `/name`, `--name` / `-n` or `pi.setSessionName()`.)

```json
{"type":"session_info","id":"k1l2m3n4","parentId":"j0k1l2m3","timestamp":"2024-12-03T14:35:00.000Z","seq":31,"name":"Refactor auth module"}
```

## Tree Structure

Entries normally form one tree, but navigation APIs can create multiple roots:
- A root entry has `parentId: null`; the first entry is initially the root
- Each non-root entry points to its parent via `parentId`
- Branching creates new children from an earlier entry
- The "leaf" is the current position in the tree
- Calling `resetLeaf()` or `branchWithSummary(null, ...)` allows a later entry to become another root

```
[user msg] ─── [assistant] ─── [user msg] ─── [assistant] ─┬─ [user msg] ← current leaf
                                                            │
                                                            └─ [branch_summary] ─── [user msg] ← alternate branch
```

## Context Building

`buildContextEntries()` walks from the current leaf to the root, producing the active entry list while honoring compaction:

1. Collects all entries on the path
2. If one or more `CompactionEntry` values are on the path, uses the latest one:
   - Includes the compaction entry first
   - Includes non-system entries from `firstKeptEntryId` up to, but not including, the compaction entry
   - Includes entries after the compaction entry
3. Preserves non-message entries in the selected range so interactive mode can render them

`buildSessionContext()` builds on that entry list to produce the message list for the LLM:

1. Extracts current model and thinking level settings from the full path
2. Converts selected entries to messages:
   - `message` -> stored `AgentMessage`
   - `compaction` -> complete system checkpoint followed by `compactionSummary`
   - `branch_summary` -> `branchSummary`
   - `custom_message` -> `CustomMessage`
   - `custom` -> no context message

The compaction summary replaces entries before `firstKeptEntryId`. Pre-compaction system messages are folded into the complete checkpoint rather than replayed from the retained range. Retained non-system entries and all entries after the compaction remain available to the LLM.

## Parsing Example

```typescript
import { readFileSync } from "fs";

const lines = readFileSync("session.jsonl", "utf8").trim().split("\n");

for (const line of lines) {
  const entry = JSON.parse(line);

  switch (entry.type) {
    case "session":
      console.log(`Session v${entry.version ?? 1}: ${entry.id}`);
      break;
    case "message":
      console.log(`[${entry.id}] ${entry.message.role}: ${JSON.stringify(entry.message.content)}`);
      break;
    case "compaction":
      console.log(`[${entry.id}] Compaction: ${entry.tokensBefore} tokens summarized`);
      break;
    case "branch_summary":
      console.log(`[${entry.id}] Branch from ${entry.fromId}`);
      break;
    case "custom":
      // customType starting with "pirs." is one of the loop server's own
      // events (see "The `pirs.*` custom entries").
      console.log(`[${entry.id}] Custom (${entry.customType}): ${JSON.stringify(entry.data)}`);
      break;
    case "custom_message":
      console.log(`[${entry.id}] Extension message (${entry.customType}): ${entry.content}`);
      break;
    case "label":
      console.log(`[${entry.id}] Label "${entry.label}" on ${entry.targetId}`);
      break;
    case "model_change":
      console.log(`[${entry.id}] Model: ${entry.provider}/${entry.modelId}`);
      break;
    case "thinking_level_change":
      console.log(`[${entry.id}] Thinking: ${entry.thinkingLevel}`);
      break;
  }
}
```

## SessionManager API (pi's)

pi's TypeScript API over these files, kept here as the reference for what the format
supports. pirs's `SessionManager` in `crates/pirs-server/src/session.rs` mirrors it in Rust
— same entry types, same tree, same context building, plus `seq` — and is internal to the
server: a pirs client reaches a conversation through the protocol, not through an API.

### Static Creation Methods
- `SessionManager.create(cwd, sessionDir?, options?)` - New session; `options` can set `id` and `parentSession`
- `SessionManager.open(path, sessionDir?, cwdOverride?)` - Open existing session file
- `SessionManager.continueRecent(cwd, sessionDir?)` - Continue most recent or create new
- `SessionManager.inMemory(cwd?, options?, entries?)` - No file persistence, optionally initialized from entries
- `SessionManager.forkFrom(sourcePath, targetCwd, sessionDir?, options?)` - Fork session from another project

### Static Listing Methods
- `SessionManager.list(cwd, sessionDir?, onProgress?)` - List sessions for a directory
- `SessionManager.listAll(onProgress?)` - List all sessions across all projects
- `SessionManager.listAll(sessionDir?, onProgress?)` - List sessions from a custom session root

### Instance Methods - Session Management
- `newSession(options?)` - Start a new session (options: `{ id?: string, parentSession?: string }`)
- `setSessionFile(path)` - Switch to a different session file
- `createBranchedSession(leafId)` - Extract branch to new session file

### Instance Methods - Appending (all return entry ID)
- `appendMessage(message)` - Add message
- `appendThinkingLevelChange(level)` - Record thinking change
- `appendModelChange(provider, modelId)` - Record model change
- `appendCompaction(summary, firstKeptEntryId, tokensBefore, details?, fromHook?, usage?)` - Add compaction
- `appendCustomEntry(customType, data?)` - Extension state (not in context)
- `appendSessionInfo(name)` - Set session display name
- `appendCustomMessageEntry(customType, content, display, details?)` - Extension message (in context)
- `appendLabelChange(targetId, label)` - Set/clear label

### Instance Methods - Tree Navigation
- `getLeafId()` - Current position
- `getLeafEntry()` - Get current leaf entry
- `getEntry(id)` - Get entry by ID
- `getBranch(fromId?)` - Walk from entry to root
- `getTree()` - Get full tree structure
- `getChildren(parentId)` - Get direct children
- `getLabel(id)` - Get label for entry
- `branch(entryId)` - Move leaf to earlier entry
- `resetLeaf()` - Reset leaf to null (before any entries)
- `branchWithSummary(entryId, summary, details?, fromHook?, usage?)` - Branch with context summary; `entryId` may be `null` to branch from the root

### Instance Methods - Context & Info
- `buildContextEntries()` - Get active branch entries with compaction applied
- `buildSessionContext()` - Get messages, thinkingLevel, and model for LLM
- `getEntries()` - All entries (excluding header)
- `getHeader()` - Session header metadata
- `getSessionName()` - Get display name from latest session_info entry
- `getCwd()` - Working directory
- `getSessionDir()` - Session storage directory
- `getSessionId()` - Session UUID
- `getSessionFile()` - Session file path (undefined for in-memory)
- `isPersisted()` - Whether session is saved to disk
