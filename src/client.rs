//! Commands run from the machine you work on (the client).
//!
//! These drive a host over SSH. The only host setting is an SSH destination,
//! normally an alias in `~/.ssh/config`, so plain `ssh` and `git` work against
//! it too. Work moves through the project's bare repository on the host:
//!
//! - `up` pushes the base commit to `refs/bases/<name>` there, so a detached
//!   HEAD (as in `jj`) works.
//! - `pull` fetches the run's branch and its snapshot of uncommitted work into
//!   `refs/remotes/corral/`.

use crate::config::Config;
use crate::git::{self, git};
use crate::layout::validate;
use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

/// Added to Claude's system prompt in every run.
const CLAUDE_NOTE: &str = "You are running unattended in an isolated \
    container. Commit your work to the current git branch as you go and \
    before you finish: only commits are collected, and you cannot push. \
    Reference files may be in /corral/inbox (read-only).";

/// Where a detached `claude` writes its output, inside the container.
const CLAUDE_LOG: &str = "/tmp/corral-claude.log";

/// Starts the command given to it in the background and returns at once.
const DETACH: &str = r#"nohup "$@" </dev/null >/tmp/corral-claude.log 2>&1 &"#;

/// Name of the git remote pointing at the host.
const REMOTE: &str = "corral";

/// A host and the project to work on.
#[derive(Debug)]
pub struct Client {
    /// SSH destination.
    host: String,

    /// Command that runs `corral` on the host.
    remote_command: String,

    /// Project name given explicitly, if any.
    project: Option<String>,
}

impl Client {
    /// Build a client from flags and the config file.
    ///
    /// # Errors
    ///
    /// Returns an error if no host is configured.
    pub fn new(host: Option<&str>, project: Option<&str>) -> Result<Self> {
        let config = Config::load()?;
        let host = host
            .map(str::to_owned)
            .or(config.host)
            .context("no host: use --host, CORRAL_HOST, or `corral init`")?;
        if host.starts_with('-') {
            bail!("invalid host: {host}");
        }
        Ok(Self {
            host,
            remote_command: config
                .remote_command
                .unwrap_or_else(|| "corral".to_owned()),
            project: project.map(str::to_owned),
        })
    }

    /// The project: the flag, `corral.project` in git config, or the name of
    /// the repository's directory.
    fn project(&self) -> Result<String> {
        if let Some(project) = &self.project {
            return Ok(project.clone());
        }
        if let Ok(project) =
            git::output(git().args(["config", "--get", "corral.project"]))
        {
            return Ok(project);
        }
        let top = git::output(git().args(["rev-parse", "--show-toplevel"]))
            .context("not in a git repository; use --project")?;
        PathBuf::from(top)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .context("could not determine a project name; use --project")
    }

    /// An `ssh` command that runs `corral host <args>` on the host.
    fn ssh(&self, tty: bool, args: &[&str]) -> Command {
        let remote = std::iter::once(self.remote_command.clone())
            .chain(["host".to_owned()])
            .chain(args.iter().map(|arg| shell_quote(arg)))
            .collect::<Vec<_>>()
            .join(" ");
        let mut command = Command::new("ssh");
        if tty {
            command.arg("-t");
        }
        command.arg(&self.host).arg(remote);
        command
    }

    /// Run a host command, failing if it does.
    fn run(&self, args: &[&str]) -> Result<()> {
        let status = self
            .ssh(false, args)
            .status()
            .context("could not run ssh")?;
        if !status.success() {
            bail!("host command `{}` failed ({status})", args.join(" "));
        }
        Ok(())
    }

    /// Run a host command and return its standard output.
    fn capture(&self, args: &[&str]) -> Result<String> {
        git::output(&mut self.ssh(false, args))
    }

    /// Fail unless the git remote for the host exists.
    fn require_remote() -> Result<()> {
        git::run(git().args(["remote", "get-url", REMOTE])).context(
            "this repository has no `corral` remote; run `corral new` first",
        )
    }

    /// Create the project on the host and add a `corral` git remote.
    ///
    /// # Errors
    ///
    /// Returns an error if the project exists or a step fails.
    pub fn new_project(&self) -> Result<()> {
        let project = self.project()?;
        self.run(&["new", &project])?;
        let path = self.capture(&["path", &project])?;
        let url = format!("{}:{path}", self.host);
        git::run(git().args(["remote", "add", REMOTE, &url]))
            .context("could not add the `corral` remote")?;
        if git::output(git().args(["config", "--get", "corral.project"]))
            .is_err()
        {
            git::run(git().args(["config", "corral.project", &project]))?;
        }
        Ok(())
    }

    /// Push `base` to the host and start a run from it.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is invalid or a step fails.
    pub fn up(&self, name: &str, base: &str) -> Result<()> {
        validate("run", name)?;
        let project = self.project()?;
        Self::require_remote()?;
        let commit = git::output(git().args([
            "rev-parse",
            "--verify",
            &format!("{base}^{{commit}}"),
        ]))
        .with_context(|| format!("no such revision: {base}"))?;

        let base_ref = format!("refs/bases/{name}");
        git::run(git().args([
            "push",
            "-q",
            REMOTE,
            &format!("+{commit}:{base_ref}"),
        ]))
        .context("could not push the base commit to the host")?;
        self.run(&["up", &project, name, "--base", &base_ref])
    }

    /// List runs on the host.
    ///
    /// # Errors
    ///
    /// Returns an error if the host command fails.
    pub fn ls(&self) -> Result<()> {
        self.run(&["ls"])
    }

    /// Run a command in a run's container, returning its exit status.
    ///
    /// # Errors
    ///
    /// Returns an error if ssh cannot be run.
    pub fn exec(&self, name: &str, command: &[String]) -> Result<ExitCode> {
        let project = self.project()?;
        let tty =
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        let mut args = vec!["exec", &project, name, "--"];
        args.extend(command.iter().map(String::as_str));
        let status =
            self.ssh(tty, &args).status().context("could not run ssh")?;
        Ok(status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from))
    }

    /// Bring a run's branch and any snapshot of uncommitted work here.
    ///
    /// # Errors
    ///
    /// Returns an error if the host sync or a fetch fails.
    pub fn pull(&self, name: &str, out: &mut dyn Write) -> Result<()> {
        validate("run", name)?;
        let project = self.project()?;
        Self::require_remote()?;
        self.run(&["sync", &project, name])?;

        let branch = format!("agent/{name}");
        git::run(git().args([
            "fetch",
            "-q",
            REMOTE,
            &format!("+refs/heads/{branch}:refs/remotes/{REMOTE}/{branch}"),
        ]))?;
        writeln!(out, "branch:   {REMOTE}/{branch}")?;

        let snapshot = format!("refs/snapshots/{name}");
        if git::run(git().args(["ls-remote", "--exit-code", REMOTE, &snapshot]))
            .is_ok()
        {
            git::run(git().args([
                "fetch",
                "-q",
                REMOTE,
                &format!("+{snapshot}:refs/remotes/{REMOTE}/snapshots/{name}"),
            ]))?;
            writeln!(
                out,
                "snapshot: {REMOTE}/snapshots/{name} (uncommitted work)"
            )?;
        }
        Ok(())
    }

    /// Run Claude Code in a run, in the foreground or detached.
    ///
    /// # Errors
    ///
    /// Returns an error if ssh cannot be run.
    pub fn claude(
        &self,
        name: &str,
        detach: bool,
        args: &[String],
    ) -> Result<ExitCode> {
        let claude = [
            "claude",
            "--dangerously-skip-permissions",
            "--append-system-prompt",
            CLAUDE_NOTE,
        ]
        .map(str::to_owned)
        .into_iter()
        .chain(args.iter().cloned());

        if detach {
            let command: Vec<String> = ["sh", "-c", DETACH, "corral-detach"]
                .map(str::to_owned)
                .into_iter()
                .chain(claude)
                .collect();
            let code = self.exec(name, &command)?;
            eprintln!("started; follow it with `corral logs -f {name}`");
            Ok(code)
        } else {
            self.exec(name, &claude.collect::<Vec<_>>())
        }
    }

    /// Show the output of a detached `claude`.
    ///
    /// # Errors
    ///
    /// Returns an error if ssh cannot be run.
    pub fn logs(&self, name: &str, follow: bool) -> Result<ExitCode> {
        let flag = if follow { "-f" } else { "-n200" };
        self.exec(
            name,
            &["tail".to_owned(), flag.to_owned(), CLAUDE_LOG.to_owned()],
        )
    }

    /// Send files or directories to a run's inbox.
    ///
    /// # Errors
    ///
    /// Returns an error if a path cannot be read or the host rejects it.
    pub fn put(&self, name: &str, paths: &[PathBuf]) -> Result<()> {
        validate("run", name)?;
        let project = self.project()?;
        let mut child = self
            .ssh(false, &["put", &project, name])
            .stdin(Stdio::piped())
            .spawn()
            .context("could not run ssh")?;
        {
            let stdin = child.stdin.take().context("no ssh stdin")?;
            let mut archive = tar::Builder::new(stdin);
            // Send symlinks as symlinks rather than what they point at.
            archive.follow_symlinks(false);
            for path in paths {
                let entry = path.file_name().with_context(|| {
                    format!("no name in {}", path.display())
                })?;
                if path.is_dir() {
                    archive.append_dir_all(entry, path)
                } else {
                    archive.append_path_with_name(path, entry)
                }
                .with_context(|| format!("could not add {}", path.display()))?;
            }
            archive.finish()?;
        }
        if !child.wait()?.success() {
            bail!("the host rejected the files");
        }
        Ok(())
    }

    /// Set a secret from standard input, or prompt for it.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be read or the host rejects it.
    pub fn secret_set(&self, name: &str) -> Result<()> {
        let project = self.project()?;
        let value = if std::io::stdin().is_terminal() {
            rpassword::prompt_password(format!("{name}: "))?.into_bytes()
        } else {
            let mut value = Vec::new();
            std::io::stdin().read_to_end(&mut value)?;
            value
        };
        let mut child = self
            .ssh(false, &["secret-set", &project, name])
            .stdin(Stdio::piped())
            .spawn()
            .context("could not run ssh")?;
        child
            .stdin
            .take()
            .context("no ssh stdin")?
            .write_all(&value)?;
        if !child.wait()?.success() {
            bail!("the host rejected the secret");
        }
        Ok(())
    }

    /// List the project's secret names.
    ///
    /// # Errors
    ///
    /// Returns an error if the host command fails.
    pub fn secret_ls(&self) -> Result<()> {
        self.run(&["secret-ls", &self.project()?])
    }

    /// Remove a secret.
    ///
    /// # Errors
    ///
    /// Returns an error if the host command fails.
    pub fn secret_rm(&self, name: &str) -> Result<()> {
        self.run(&["secret-rm", &self.project()?, name])
    }

    /// Remove a run on the host. Its branch stays in the project.
    ///
    /// # Errors
    ///
    /// Returns an error if the host command fails.
    pub fn rm(&self, name: &str) -> Result<()> {
        validate("run", name)?;
        let project = self.project()?;
        self.run(&["rm", &project, name])
    }
}

/// Set up SSH access to a host: a key, an `~/.ssh/config` entry, and the
/// default host in corral's config. Prints what to add on the host.
///
/// # Errors
///
/// Returns an error if a file cannot be written or `ssh-keygen` fails.
pub fn init(
    name: &str,
    address: &str,
    user: &str,
    no_passphrase: bool,
    out: &mut dyn Write,
) -> Result<()> {
    validate("host", name)?;
    let home = PathBuf::from(std::env::var_os("HOME").context("no HOME")?);
    let ssh_dir = home.join(".ssh");
    std::fs::create_dir_all(&ssh_dir)?;
    std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700))?;

    let alias = format!("corral-{name}");
    let key = ssh_dir.join(&alias);
    if key.exists() {
        writeln!(out, "using existing key {}", key.display())?;
    } else {
        let mut keygen = Command::new("ssh-keygen");
        keygen.args(["-t", "ed25519", "-C", &alias, "-f"]).arg(&key);
        if no_passphrase {
            keygen.args(["-N", ""]);
        }
        if !keygen
            .status()
            .context("could not run ssh-keygen")?
            .success()
        {
            bail!("ssh-keygen failed");
        }
    }

    let config_path = ssh_dir.join("config");
    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    if existing
        .lines()
        .any(|line| line.trim() == format!("Host {alias}"))
    {
        writeln!(out, "ssh config already has `Host {alias}`")?;
    } else {
        let entry = format!(
            "\n# Added by `corral init`\nHost {alias}\n  HostName {address}\n  \
             User {user}\n  IdentityFile {}\n  IdentitiesOnly yes\n",
            key.display()
        );
        let created = !config_path.exists();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config_path)?;
        file.write_all(entry.as_bytes())?;
        if created {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        writeln!(out, "added `Host {alias}` to {}", config_path.display())?;
    }

    let mut config = Config::load()?;
    if config.host.is_none() {
        config.host = Some(alias.clone());
        config.save()?;
        writeln!(out, "default host is now {alias}")?;
    }

    let public = std::fs::read_to_string(format!("{}.pub", key.display()))?;
    writeln!(
        out,
        "\nAdd this to the host's NixOS configuration, then rebuild:\n\n  \
         services.agents.authorizedKeys = [\n    \
         \"no-agent-forwarding,no-X11-forwarding {}\"\n  ];\n\n\
         Then check access with: ssh {alias}",
        public.trim()
    )?;
    Ok(())
}

/// Quote `arg` for a POSIX shell, if it needs it.
fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        arg.to_owned()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("abc-1.2/x"), "abc-1.2/x");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("$(id)"), "'$(id)'");
        assert_eq!(shell_quote("a;b"), "'a;b'");
    }

    #[test]
    fn remote_command_is_assembled_and_quoted() {
        let client = Client {
            host: "h".to_owned(),
            remote_command: ". ~/env; corral".to_owned(),
            project: None,
        };
        let command =
            client.ssh(false, &["exec", "p", "n", "--", "sh -c", "x"]);
        let args: Vec<_> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["h", ". ~/env; corral host exec p n -- 'sh -c' x"]);
    }
}
