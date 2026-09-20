# pirs — north star

pirs is a tool for working with coding agents. Three wants shape it: run many agents at
once, on more than one machine when one is not enough; see them all in one place and know
at a glance who needs you; and shape the tool as easily as asking an agent to do it. The
first two must never be paid for with the third. If trying a customisation ever takes more
than asking, something below this page is wrong.

## What we are betting on

**Most of what people want from an agent tool is policy, not code.** What to put in front
of the model, what to run after each turn, how to react to what you type. A policy is better
written as a short declaration than as a program: it is easier to read, to check, to share
and to have the model write for you. The evidence is not any agent tool's plugin gallery but
the customisations people demonstrably use: permission rules, hooks, instruction files.

**The right thing to build is agents behind a door, not an agent inside a terminal.** One or
more agent loops run in a server that owns them. Your UI is one visitor at that door, a
script is another, and another agent is a third. Because the server owns the loop, it knows
whether an agent is working, waiting on you, or done, and says so as a fact. Anything that
sits outside the loop can only guess from what is on the screen. pirs will sit inside
whatever multiplexer or window system you already use, and that stays true there.

## Principles

1. **The core is what would hurt to change.** Only the parts whose interface we promise.
2. **Everything outside the core is a peer.** The UI, a script, another agent and your own
   tools all come through the same door, and none of them lives inside.
3. **Most customisation is a declaration, not a program.**
4. **Every customisation carries its own intent in words** and can be rebuilt from it.
5. **Customisations combine by fixed rules** you can check before running anything.
6. **When a declaration cannot say it, a program in any language can.**
7. **Status is a fact, not a guess.**
8. **The core holds only what needs the loop.** Terminals, editors and windows are someone
   else's, and stay buildable.

## The picture

```
        your UI          a script        another agent      your own tools
            \               |                 |                /
             ─────────────── the same door ───────────────────
                                  │
               ┌──────────────────┴──────────────────┐
               │            pirs server               │
               │                                      │
               │   an agent   an agent   an agent     │
               │                                      │
               │   each one: a conversation, a        │
               │   directory, a model, its tools,     │
               │   and the policy that applies to it  │
               └──────────────────┬──────────────────┘
                                  │
                       the model, over the network
```

The server has no screen and never draws anything. It keeps running when you close your UI.
It can run on another machine, or inside a container you do not trust the work in, and your
UI looks the same either way.

## What pirs is not

- **Not a guard.** pirs does no checking inside the loop of what the model may do, and ships
  nothing that could be mistaken for such a check. If you need containment, put the server
  inside a boundary you trust and attach from outside. The argument is D-19.
- **Not an editor, a terminal, or a window manager.** It uses yours. The argument is
  principle 8, and D-20 records why a server rather than tmux.
- **Not pi.** pirs began as a port of pi and keeps pi's minimal-core argument, but its
  customisations are declarations and separate programs, not code loaded into the tool.
  Existing pi extensions do not run. The argument is D-22.

Everything the model receives can be seen: what a policy adds to the prompt, and what it
changes in a tool's output. The argument is D-21.
