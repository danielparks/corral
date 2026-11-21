//! Code to deal with executable parameters.
#![allow(clippy::allow_attributes, reason = "framework code from a template")]

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use termcolor::{Color, ColorSpec, StandardStream, WriteColor};

pub use clap::Parser;

/// Parameters to configure executable.
#[derive(Debug, clap::Parser)]
#[clap(version, about)]
pub struct Params {
    /// Whether or not to output in color.
    #[clap(long, default_value = "auto", value_name = "WHEN")]
    pub color: ColorChoice,

    /// Verbosity (may be repeated up to three times).
    #[clap(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// What to do.
    #[clap(subcommand)]
    pub command: Command,
}

/// Subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Check a devcontainer.json against the policy for runs.
    ///
    /// Run from the repository root.
    CheckConfig {
        /// Path to devcontainer.json, relative to the current directory.
        path: PathBuf,
    },

    /// Create a project: a bare repository that runs clone from.
    New {
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

        /// Branch or commit to start from.
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

    /// Run `podman` after checking its arguments against the run policy.
    ///
    /// This is the devcontainer CLI's `--docker-path`. The policy comes from
    /// `CORRAL_BIND_ROOTS` and `CORRAL_ORIGIN`; the engine from
    /// `CORRAL_PODMAN`.
    #[clap(hide = true)]
    PodmanShim {
        /// Arguments for podman.
        #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
