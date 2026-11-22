//! Code to deal with executable parameters.
#![allow(clippy::allow_attributes, reason = "framework code from a template")]

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

pub use clap::Parser;

/// Run coding agents in containers on a remote host.
///
/// Work moves between this machine (the client) and the host as git
/// branches. Each run gets a clone on the host and its own container.
#[derive(Debug, clap::Parser)]
#[clap(version, about)]
pub struct Params {
    /// Whether or not to output in color.
    #[clap(long, default_value = "auto", value_name = "WHEN")]
    pub color: ColorChoice,

    /// Verbosity (may be repeated up to three times).
    #[clap(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// SSH host to use, usually an alias in `~/.ssh/config`.
    ///
    /// Defaults to the host in the config file.
    #[clap(long, global = true, env = "CORRAL_HOST", value_name = "HOST")]
    pub host: Option<String>,

    /// Project name. Defaults to `corral.project` in git config, or the
    /// repository's directory name.
    #[clap(
        long = "project",
        global = true,
        env = "CORRAL_PROJECT",
        value_name = "NAME"
    )]
    pub project_name: Option<String>,

    /// What to do.
    #[clap(subcommand)]
    pub command: Command,
}

/// Subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Set up SSH access to a host: a key, an `~/.ssh/config` entry, and the
    /// default host.
    Init {
        /// Short name for the host, e.g. `nellie`.
        name: String,

        /// Address to connect to.
        address: String,

        /// User to log in as on the host.
        #[clap(long, default_value = "agent")]
        user: String,

        /// Do not protect the new key with a passphrase.
        #[clap(long)]
        no_passphrase: bool,
    },

    /// Create this repository's project on the host and add a `corral`
    /// remote.
    New,

    /// Start a run on the host, on a new branch `agent/<name>`.
    Up {
        /// Run name.
        name: String,

        /// Revision to start from. Works with a detached HEAD.
        #[clap(long, default_value = "HEAD")]
        base: String,
    },

    /// List runs on the host.
    Ls,

    /// Run a command inside a run's container.
    Exec {
        /// Run name.
        name: String,

        /// Command and arguments.
        #[clap(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Fetch a run's branch (and any snapshot of uncommitted work) from the
    /// host.
    Pull {
        /// Run name.
        name: String,
    },

    /// Send files, images, or whole repositories to a run.
    ///
    /// They appear read-only under `/corral/inbox` in the container, each
    /// under its own name.
    Put {
        /// Run name.
        name: String,

        /// Files or directories to send.
        #[clap(required = true)]
        paths: Vec<PathBuf>,
    },

    /// Manage the project's secrets, which are exported as environment
    /// variables in `exec` and never stored in git or an image.
    Secret {
        /// What to do.
        #[clap(subcommand)]
        command: SecretCommand,
    },

    /// Remove a run's container and files. Its branch stays in the project.
    Rm {
        /// Run name.
        name: String,
    },

    /// Commands that run on the host. The client runs these over SSH.
    Host {
        /// What to do on the host.
        #[clap(subcommand)]
        command: HostCommand,
    },

    /// Check a devcontainer.json against the policy for runs.
    ///
    /// Run from the repository root.
    CheckConfig {
        /// Path to devcontainer.json, relative to the current directory.
        path: PathBuf,
    },

    /// Run `podman` after checking its arguments against the run policy.
    ///
    /// This is the devcontainer CLI's `--docker-path`. The policy comes from
    /// `CORRAL_BIND_ROOTS` and `CORRAL_RO_MOUNTS`; the engine from
    /// `CORRAL_PODMAN`.
    #[clap(hide = true)]
    PodmanShim {
        /// Arguments for podman.
        #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// What to do with secrets.
#[derive(Debug, clap::Subcommand)]
pub enum SecretCommand {
    /// Set a secret from standard input, or prompt for it.
    Set {
        /// Name, which becomes the environment variable name.
        name: String,
    },

    /// List secret names.
    Ls,

    /// Remove a secret.
    Rm {
        /// Name.
        name: String,
    },
}

/// Commands that run on the host.
#[derive(Debug, clap::Subcommand)]
pub enum HostCommand {
    /// Unpack a tar archive from standard input into a run's inbox.
    Put {
        /// Project name.
        project: String,

        /// Run name.
        name: String,
    },

    /// Set a secret from standard input.
    SecretSet {
        /// Project name.
        project: String,

        /// Secret name.
        name: String,
    },

    /// List a project's secret names.
    SecretLs {
        /// Project name.
        project: String,
    },

    /// Remove a secret.
    SecretRm {
        /// Project name.
        project: String,

        /// Secret name.
        name: String,
    },

    /// Create a project: a bare repository that runs clone from.
    New {
        /// Project name.
        project: String,
    },

    /// Print the path of a project's bare repository.
    Path {
        /// Project name.
        project: String,
    },

    /// Start a run on a new branch `agent/<name>`.
    ///
    /// The devcontainer config is read from the base commit, not from the
    /// agent's workspace.
    Up {
        /// Project name.
        project: String,

        /// Run name.
        name: String,

        /// Branch, ref, or commit in the project to start from.
        #[clap(long, default_value = "main")]
        base: String,
    },

    /// List runs.
    Ls,

    /// Run a command inside a run's container.
    Exec {
        /// Project name.
        project: String,

        /// Run name.
        name: String,

        /// Command and arguments.
        #[clap(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },

    /// Fetch a run's branch into the project, saving uncommitted work as a
    /// snapshot.
    Sync {
        /// Project name.
        project: String,

        /// Run name.
        name: String,
    },

    /// Remove a run's container and files.
    Rm {
        /// Project name.
        project: String,

        /// Run name.
        name: String,
    },
}

impl Params {
    /// Print a warning message in error color to `err_stream()`.
    pub fn warn<S: AsRef<str>>(&self, message: S) -> io::Result<()> {
        let mut err_out = self.err_stream();
        err_out.set_color(&error_color())?;
        err_out.write_all(message.as_ref().as_bytes())?;
        err_out.reset()?;

        Ok(())
    }

    /// Get stream to use for standard output.
    #[allow(dead_code, reason = "framework code")]
    pub fn out_stream(&self) -> StandardStream {
        StandardStream::stdout(self.color_choice(&io::stdout()))
    }

    /// Get stream to use for errors.
    pub fn err_stream(&self) -> StandardStream {
        StandardStream::stderr(self.color_choice(&io::stderr()))
    }

    /// Whether or not to output on a stream in color.
    ///
    /// Checks if passed stream is a terminal.
    pub fn color_choice<T: IsTerminal>(
        &self,
        stream: &T,
    ) -> termcolor::ColorChoice {
        if self.color == ColorChoice::Auto && !stream.is_terminal() {
            termcolor::ColorChoice::Never
        } else {
            self.color.into()
        }
    }
}

/// Whether or not to output in color.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Output in color when running in a terminal that supports it.
    #[default]
    Auto,

    /// Always output in color.
    Always,

    /// Never output in color.
    Never,
}

impl From<ColorChoice> for termcolor::ColorChoice {
    fn from(choice: ColorChoice) -> Self {
        match choice {
            ColorChoice::Auto => Self::Auto,
            ColorChoice::Always => Self::Always,
            ColorChoice::Never => Self::Never,
        }
    }
}

/// Returns color used to output errors.
pub fn error_color() -> ColorSpec {
    let mut color = ColorSpec::new();
    color.set_fg(Some(Color::Red));
    color.set_intense(true);
    color
}
