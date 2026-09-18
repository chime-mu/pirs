# Handoff

Written 2026-09-18 at shutdown. Read this first, then `STATUS.md` for the feature inventory
and `docs/extensions.md` for the extension API.

## Where things stand

- `pirs` is a working Rust port of pi: agent loop, seven built-in tools, pi-compatible sessions,
  Anthropic and OpenAI streaming, print/JSON/interactive modes, and an embedded QuickJS host
  that runs pi's TypeScript extensions unchanged (71 of pi's 77 examples load).
- Live Anthropic requests work through the user's Claude Code login (keychain). The OpenAI
  provider is unit-tested only.
- The model can document itself: the system prompt has a `<docs>` section pointing at
  `README.md`, `docs/`, `examples/`, and `STATUS.md`, and the user has already had pirs write
  and test an extension (`secrets-protection.ts`, see below).
- All 85 tests pass (`cargo test --workspace`). Last commit on `main`: see `git log`.

## Build and run

```bash
cargo build --release
./target/release/pirs                      # interactive, uses Claude Code login if no key set
./target/release/pirs -p "prompt"          # print mode
./target/release/pirs --list-models        # shows which credential source is active
./target/release/pirs --list-extensions -e examples/extensions/todo.ts
PIRS_FAUX_SCRIPT=script.json ./target/release/pirs -p --model faux/scripted "x"   # offline
```

## Untracked files in the working tree

These were produced by the user's own pirs session (the agent building an extension for
itself) and are intentionally not committed. Decide whether to keep, move, or delete them:

- `secrets-protection.ts`: an extension blocking writes under `secrets/` and confirming
  `git push`. Working; a candidate for `examples/extensions/`.
- `test-secrets-automated.ts`, `test-secrets-extension.sh`, `run-extension-tests.sh`,
  `TEST_RESULTS.md`: its tests and results.
- `secrets/`, `test-workspace/`, `fake-remote/`: scratch fixtures for those tests.
- `PI_INSTALLATION.md`, `RIPGREP_FD_EXPLAINED.md`: notes written during that session
  (the first suggests the original pi was installed on this machine for comparison).

## Things that will bite the next person

- **Reference pi checkout is gone.** During development pi was cloned into a temporary
  scratch directory. `crates/pi-ext/src/lib.rs` (`examples_dir()`) and
  `crates/pi-ext/examples/sweep.rs` refer to that path; the affected test skips when the
  directory is missing. To restore: `git clone https://github.com/earendil-works/pi` somewhere
  stable and point the path at `<pi>/packages/coding-agent/examples/extensions`, or make it an
  env var (`PI_EXAMPLES_DIR`), which is the better fix.
- **Never query the terminal cursor in the TUI** while crossterm's `EventStream` exists; it
  blocks for two seconds and corrupts the inline viewport. `TrackedBackend` in
  `crates/pi-cli/src/modes/interactive.rs` exists for this reason. ratatui is built with the
  `scrolling-regions` feature for the same reason.
- **Extension host concurrency**: one `ctx.async_with` block owns the QuickJS runtime and every
  request is `ctx.spawn`ed onto rquickjs's scheduler (`crates/pi-ext/src/host.rs`,
  `run_host`). Do not await host requests while holding the runtime lock elsewhere, and do not
  add a second `async_with` block; it would serialize with the first and deadlock on dialogs.
- **All JS-facing behaviour lives in `crates/pi-ext/src/js/runtime.js`** (the `pi` API, `ctx`,
  dispatch semantics) and the shims in `crates/pi-ext/src/js/*.js`. Rust only provides the
  `__hostSync`/`__hostAsync` natives and the `HostCallbacks` trait. Add new host functions in
  `host_sync`/`host_async` in `host.rs` and call them from JS.
- **OAuth**: `crates/pi-ai/src/oauth.rs`. Sources are tried in order (pi `auth.json`, Claude
  Code file, keychain); expired file tokens are refreshed and written back; keychain tokens are
  never refreshed (Claude Code owns them). A stale `~/.claude/.credentials.json` from June exists
  on this machine and is skipped.
- **Session files** go to `~/.pi/agent/sessions/<encoded cwd>/`, the same place pi uses; pirs
  and pi can open each other's files. `--no-session` for throwaway runs.
- `PIRS_TRACE=1` prints dispatch timings to stderr; useful for latency questions.

## Suggested next work (in order)

1. Move the pi examples path to an env var and add a CI-friendly fixture set.
2. Compaction (auto and `/compact`), then `/tree` and `/fork` in the TUI. The session manager
   already supports branching, forking, and compaction entries.
3. Bind `pi.registerShortcut` keys in the TUI and parse `pi.registerFlag` flags from the CLI.
4. Markdown rendering and expandable tool output in the TUI.
5. Skills and prompt templates (`/skill:`, `/template`).
6. Live-test the OpenAI provider; add Google/Responses APIs if needed.
7. Consider promoting `secrets-protection.ts` to `examples/extensions/` after review.

## Layout reminder

```
crates/pi-ai      providers (anthropic.rs, openai.rs, faux.rs), registry.rs, oauth.rs, types.rs
crates/pi-agent   agent_loop.rs, agent.rs, types.rs (AgentTool, events, hooks), validate.rs
crates/pi-ext     host.rs (thread, requests, callbacks), loader.rs, strip.rs, js/runtime.js, js/*.js shims
crates/pi-cli     main.rs (clap), agent_session.rs (glue: hooks + callbacks), tools/, session.rs,
                  settings.rs, system_prompt.rs, modes/print.rs, modes/interactive.rs
docs/             extensions.md (pirs), pi-extensions-reference.md, session-format.md
examples/extensions/  seven pi examples that load in pirs
```
