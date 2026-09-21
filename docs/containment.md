# Running the agent in a jail

The server/client split is the security model, and the only one pirs has.

Everything a loop does — built-in tools, `run =` shell strings, extension executables, file
access — happens where the server runs. Put the server inside a boundary you trust and
attach from outside it:

- the UI stays on the host, with your terminal, clipboard and fonts;
- every tool and extension executes inside the boundary, because that is where the server
  is;
- the boundary is enforced by the kernel, the hypervisor or SSH, not by pirs;
- pirs checks nothing and therefore claims nothing.

This is the recommended setup for a repository or an input you do not trust. Attaching to a
jailed agent looks no different from a local one: the sidebar lists it beside the others and
a file page reads its files, because the server reads them for you.

## The bridge

A server outside this machine is reached through a *bridge*: a command that forwards
protocol lines between its stdin/stdout and the server's unix socket. `pirs proxy` is that
command; whatever you put in front of it is the transport.

`~/.pirs/servers.toml`:

```toml
[[server]]
name = "local"                              # no command: the local socket

[[server]]
name = "jail"
command = "docker exec -i jail pirs proxy"  # a container on this machine

[[server]]
name = "build"
command = "ssh build pirs proxy"            # another machine
```

Then `pirs --server jail "why does the build fail?"`, `pirs --server jail --list`, and the
UI's sidebar spans every server at once. `pirs --list` without `--server` lists them all,
each loop named `server:id`.

pirs knows nothing about SSH or containers. The command is the whole of the transport:
SSH provides the authentication and the encryption, the container runtime provides the
boundary, and the bridge itself forwards lines and holds no state (D-05, D-36). The loop
server never listens on the network and never speaks HTTP, so there is no port to audit
and no login to add — a bridge that does listen on one is a separate component you choose
to run, and it owns its own login.

`pirs stop` works on a local server only; a server in a container or on another machine is
stopped where it runs.

A bridge *does* start a server, on its own side. Every pirs client starts a server that is
not running, and over SSH or `docker exec` the proxy is the client's stand-in on that
machine: `pirs proxy` with nothing listening on the socket starts `pirs serve` detached,
waits for the socket and then forwards as usual, so a jailed or remote server need never be
started by hand — attaching is enough. `pirs proxy --no-start` turns that off and fails
instead. A container image still names `pirs serve` as its entrypoint, because a server is
the point of the image rather than something that happens when somebody attaches, and
because the image's own idle timeout is what decides how long the container lives.

Everything belonging to a loop lives on the server that runs it: its cwd, its tools, its
policy files, its session log. Personal policy for a jailed server therefore lives in that
server's home directory, not in yours (D-26). Paths in what a server sends are labels: the
client displays them and hands them back, and never opens one itself (D-31).

## A container image

`Dockerfile` in the repository root builds one:

```sh
docker build -t pirs .
docker run -d --name jail \
  -v "$PWD:/work" -w /work \
  -e ANTHROPIC_API_KEY \
  pirs
```

The build copies the source and no more — `.dockerignore` keeps `target`, `.git` and any
session log out of the image. The image runs `pirs serve` as its entrypoint on
`/pirs/pirs.sock` inside the container.
Nothing is published: the socket is reached with `docker exec`, which is what the
`servers.toml` entry above does. Mount the project you want the agent to work on, and
nothing else.

With the container running, `pirs --server jail --list` shows its agents and `pirs tui`
shows them in the sidebar.

## What the boundary does not cover

**Egress.** A jailed server holds a model API key and needs the network to reach the
provider, so the provider endpoint is the exfiltration channel: anything the agent can read
inside the boundary, it can put in a prompt. The boundary does not close this, and pirs does
not try to. Two answers, both outside pirs:

- allowlist the provider endpoint at the boundary — the container's network policy, a
  firewall rule, standard tooling for whoever runs the boundary; or
- run an HTTP proxy at the boundary that holds the credentials, and let the jail talk only
  to it. The jail then never sees the key.

Model traffic stays inside the loop server either way: moving it to the client side of the
boundary was considered and rejected, because it would split the one component that has to
stay whole.

**What you mount.** The boundary is only as small as the volumes you give it. A container
with your home directory mounted is not a jail.

**The bridge's own reach.** `docker exec` and `ssh` are the boundary's doors, and whoever
can run them has whatever those doors give. pirs adds no authentication of its own and
takes none away: the login is the one you already have.

**The model.** Nothing here checks what the model may do (D-19). Containment is about where
it happens, not about what is allowed to happen.
