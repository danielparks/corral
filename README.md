# corral

Run coding agents, such as Claude Code, unattended in containers on a remote
machine, and move the work back and forth as git branches.

The machine you work on is the **client**. The machine that runs the agents is
the **host**; it needs rootless Podman and nothing else you have to set up by
hand (see [Host setup](#host-setup)).

## How it works

- A **project** is a bare git repository on the host.
- A **run** is a clone of that project on a branch `agent/<name>`, plus its own
  container, built from the repository's `devcontainer.json`. Several runs of
  one project can go at once.
- `up` pushes a revision from your repository to the host and starts a run from
  it. A detached `HEAD` works, so this is fine with `jj`.
- `pull` brings the agent's branch back as `corral/agent/<name>`. If the agent
  left uncommitted changes, they come back as a commit on
  `corral/snapshots/<name>` instead, and never touch the branch.

Nothing is shared between client and host except git objects and the files you
send explicitly, so it makes no difference whether the host is a server, a VM, or
the same laptop.

## Quick start

```sh
corral init nellie nellie.example   # key + ssh config; prints what to add on the host
corral new                          # create this repository's project on the host
corral secret set CLAUDE_CODE_OAUTH_TOKEN   # from `claude setup-token`
corral up fix-bug                   # start a run from HEAD
corral claude fix-bug --detach -- -p "Fix the failing test in src/parser.rs"
corral logs -f fix-bug              # watch it
corral pull fix-bug                 # fetch its branch and any snapshot
git log corral/agent/fix-bug
corral rm fix-bug                   # remove the container; the branch stays
```

## Commands

Run from inside the repository, unless noted. `--host` (or `CORRAL_HOST`) picks
the SSH host and `--project` (or `CORRAL_PROJECT`) the project; otherwise they
come from `~/.config/corral/config.toml` and `corral.project` in git config,
falling back to the directory name.

| Command | What it does |
| --- | --- |
| `init <name> <address>` | Creates an SSH key and `~/.ssh/config` entry, sets the default host, and prints the line to add to the host's configuration. Not repository-specific. |
| `new` | Creates the project on the host and adds a `corral` git remote. |
| `up <run> [--base REV]` | Starts a run on branch `agent/<run>` from `REV` (default `HEAD`). |
| `ls` | Lists runs on the host and their container state. |
| `exec <run> -- CMD...` | Runs a command in the container. Secrets are in its environment. |
| `claude <run> [--detach] [-- ARGS]` | Runs Claude Code with permission prompts off and a note to commit its work. |
| `logs [-f] <run>` | Shows the output of a detached `claude`. |
| `put <run> PATH...` | Sends files, images, or whole repositories. They appear read-only under `/corral/inbox`. |
| `secret set\|ls\|rm` | Manages per-project secrets. Values come from standard input or a no-echo prompt. |
| `pull <run>` | Fetches the run's branch and any snapshot of uncommitted work. |
| `rm <run>` | Removes the container and files. The branch and snapshot stay in the project. |
| `host ...` | The commands that run on the host. The client calls these over SSH. |
| `check-config PATH` | Checks a `devcontainer.json` against the policy below. |

Claude needs credentials, which are ordinary secrets: set
`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`. Secrets are exported as
environment variables, named after the secret, for every `exec` and `claude`.

## Security model

The aim is that an agent running with permission prompts off cannot administer
the host, reach the rest of the network, or tamper with your repositories. Much
of this depends on the host setup: an unprivileged user, rootless Podman,
resource limits, and egress rules (the NixOS module does all of those).

What corral itself does:

- **The `devcontainer.json` is untrusted.** It is read from the base commit you
  pushed, checked against an allowlist, and exported to a separate directory
  that is used for the build, so edits the agent makes to its own copy have no
  effect. Properties that run commands on the host or raise privileges
  (`initializeCommand`, `mounts`, `runArgs`, `privileged`, compose files, and
  anything not yet reviewed) are rejected.
- **Every `podman` call is filtered.** The devcontainer CLI runs through a shim
  that allows only the subcommands and options it needs, and only host paths
  inside the run's own directories. Devcontainer features can add privileges
  through image metadata, which the config check cannot see; the shim can.
- **Git never runs in the agent's clone.** The agent can write anything there,
  including `.git/config` and hooks. `pull` fetches from it into the trusted
  repository, and snapshots read file contents only. Clones are made without
  hard links so a container cannot corrupt shared objects.
- **Read-only mounts.** Each container sees the project repository at
  `/srv/origin.git`, the inbox, and the project's secrets, all read-only. The
  agent can fetch your follow-up commits from `origin` but cannot push.
- **Snapshots exclude** `.env*`, keys, `node_modules/`, `target/`, and the like,
  and skip files that cannot be read.

What it does **not** do:

- Secrets are in the agent's environment, so a prompt injection can read and
  send them. Egress rules limit where they can go; an allowlist proxy would be
  better.
- All runs share one host user. Isolation between runs is the container
  boundary, not a defence against a container escape.
- There is no size limit on snapshots, only the exclude patterns.

## Host setup

On NixOS, use the `agents` module from the author's NixOS configuration
(`modules/agents.nix`); it creates the
unprivileged user, enables rootless Podman, sets resource limits and egress
rules, and installs this package. The flake's package wraps `corral` with a
pinned devcontainer CLI, git, and tar.

Elsewhere you need rootless Podman, git, tar, devcontainer CLI 0.91.x
(`CORRAL_DEVCONTAINER` names it), and `corral` on the SSH user's `PATH`. Set
`remote_command` in the client config if the host needs an environment first.

## Installation

```sh
nix build github:danielparks/corral        # or add it as a flake input
cargo install --git https://github.com/danielparks/corral
```

The minimum supported Rust version is 1.91.1.

## Development

`cargo test` covers the policy, the shim, and file handling. The parts that
need a real host (`up`, `exec`, `pull`) are checked by running them against one.

## Development status

This is in active development. I am open to [suggestions][issues].

## License

Unless otherwise noted, this project is dual-licensed under the Apache 2 and MIT
licenses. You may choose to use either.

  * [Apache License, Version 2.0](LICENSE-APACHE)
  * [MIT license](LICENSE-MIT)

### Contributions

Unless you explicitly state otherwise, any contribution you submit as defined
in the Apache 2.0 license shall be dual licensed as above, without any
additional terms or conditions.

[issues]: https://github.com/danielparks/corral/issues
