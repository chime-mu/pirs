#!/bin/sh
# A `[[render]]` hook for the `ask` tool (S6, D-37).
#
# The TUI writes one JSON line on stdin -- `{"tool","args","id"}` -- and reads
# one JSON line back: `lines` replaces the default drawing of the tool call,
# and `options` makes the TUI open a native picker whose choice it sends as
# the next prompt. A real hook would read the question and the options out of
# `args`; this one is a fixture and answers the same thing every time.
cat >/dev/null
printf '%s\n' '{"lines":["Which one?"],"options":["alpha","beta"]}'
