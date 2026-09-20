# The pirs design, in layers

Status: draft under discussion. Nothing described here is implemented.

The design is written in layers. Each layer exists only to serve a line in the layer above
it, and a decision is argued in the layer it belongs to, never alongside a decision from
another layer. If a message name or a field shows up in a north-star conversation, someone
is on the wrong layer.

| File | Layer | Lifetime |
|---|---|---|
| `00-north-star.md` | what pirs is for, in plain words | one page; changes rarely; carries no table of messages or fields |
| `10-functionality.md` | what a user can do, as scenarios | the acceptance test for everything below |
| `20-architecture.md` | components, crates, edges, mechanical checks, phases | short, because most of it becomes a test |
| `30-protocol.md` | the wire: events, slots, requests | written only as far as the next phase needs |
| `40-dsl.md` | the policy vocabulary and its composition rules | same |
| `90-decisions.md` | numbered decisions with a status and their argument | append-only; nothing is re-decided silently |

Working rules:

- A change to any layer is first an entry in `90-decisions.md` marked *proposed*. The prose
  is edited only when the entry is *accepted*. Accepted entries keep their full argument, so
  the layer files can state without arguing.
- Detail is written for the next phase only. The north star may describe phase 8; the
  protocol file may not.
- Each phase starts with a one-page brief: which scenarios it enables, which crate edges and
  protocol messages it adds, and which test proves it. The brief is approved before code is
  written, and the test is the acceptance.

`PLAN.md` is the execution plan: phase briefs, division of labour between models, the
acceptance script per phase, and what the orchestrator may decide alone. `page.py` renders
these files as one tabbed page for reading away from the repo.
