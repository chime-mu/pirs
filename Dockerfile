# A pirs loop server in a container: the boundary from docs/containment.md.
#
# The server runs here, with the project mounted at /work; the UI stays on the
# host and attaches over the socket:
#
#   docker build -t pirs .
#   docker run -d --name jail -v "$PWD:/work" -w /work -e ANTHROPIC_API_KEY pirs
#
#   ~/.pirs/servers.toml:
#     [[server]]
#     name = "jail"
#     command = "docker exec -i jail pirs proxy"
#
# Nothing is published and no port is opened: `docker exec` is the door, and
# the boundary is the container runtime's, not pirs's. What the agent can
# reach is what you mounted and what the network policy allows — see
# docs/containment.md, and the note there about egress.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p pirs

FROM debian:bookworm-slim
# The tools a coding agent reaches for through `bash`. Anything else a policy
# file needs is installed here too; the server runs what it is given and
# checks nothing.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates git \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/pirs /usr/local/bin/pirs

# pirs's own directory: the socket (/pirs/pirs.sock), the sessions, and the
# personal policy files, which belong to the machine the server runs on
# (D-26).
ENV PIRS_HOME=/pirs
RUN mkdir -p /pirs /work
VOLUME ["/work"]
WORKDIR /work

# The server, in this process, until it is stopped. The idle exit ends the
# container with it, so a jail that is meant to be there tomorrow names a
# long idle rather than the default ten minutes.
ENTRYPOINT ["pirs", "serve"]
CMD ["--idle", "86400"]
