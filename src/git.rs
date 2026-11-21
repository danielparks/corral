//! Thin wrappers for running `git` and other helper programs.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

/// A `git` command that never prompts and ignores system configuration.
#[must_use]
pub fn git() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

/// A `git` command operating on a bare repository.
///
/// Anything that must not trust a repository's own configuration should use
/// this, with the trusted repo as `repo`, rather than running git inside an
/// untrusted clone.
#[must_use]
pub fn bare(repo: &Path) -> Command {
    let mut command = git();
    command.arg("--git-dir").arg(repo);
    command
}

/// Run `command` and return its trimmed standard output.
///
/// # Errors
///
/// Returns an error including standard error if the command fails.
pub fn output(command: &mut Command) -> Result<String> {
    let out = command.output().with_context(|| {
        format!("could not run {:?}", command.get_program())
    })?;
    if !out.status.success() {
        bail!(
            "{:?} failed ({}): {}",
            command.get_program(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Run `command`, discarding its output.
///
/// # Errors
///
/// Returns an error including standard error if the command fails.
pub fn run(command: &mut Command) -> Result<()> {
    output(command).map(|_| ())
}
