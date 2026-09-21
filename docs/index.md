# pirs documentation

- [dsl.md](dsl.md): the policy vocabulary — the slots a `*.pirs.toml` under `.pirs/ext/` may fill, and what `run` gets and returns
- [protocol.md](protocol.md): the wire — one JSON object per line over a unix socket, with [protocol.schema.json](protocol.schema.json) as its machine-readable form
- `../examples/policy/`: working policy examples, one directory each, with their scripts
- [session-format.md](session-format.md): the session JSONL format shared with pi
- [design/](design/00-north-star.md): the adaptable-software design, in layers — `00-north-star`, `10-functionality`, `20-architecture`, `30-protocol`, `40-dsl`, `90-decisions`; `PLAN.md` is the execution plan
- `../README.md` and `../STATUS.md`: overview and implementation status
