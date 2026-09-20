# Acceptance scripts

One script per phase, `phase-N.sh`; each is that phase's acceptance test and what you run
to check the phase by hand.

Run one from the repository root: `scripts/acceptance/phase-0.sh`. Exit status 0 means the
phase passes; every check prints one line.

They need no network and no credentials: model calls use the faux provider.
