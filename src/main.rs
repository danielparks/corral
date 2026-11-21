//! corral executable.

use std::io::Write;
use std::process::ExitCode;

mod devcontainer;
mod logging;
mod params;

use anyhow::{Context, bail};
use params::{Command, Params, Parser};
use std::path::Path;

/// Wrapper to handle errors.
///
/// See [`cli()`].
fn main() -> ExitCode {
    let params = Params::parse();
    cli(&params).unwrap_or_else(|error| {
        tracing::debug!("Exiting with error: {error:#?}");
        let error = format!("{error}\n");
        if error.to_lowercase().starts_with("error") {
            params.warn(error).unwrap();
        } else {
            params.warn(format!("Error: {error}")).unwrap();
        }

        ExitCode::FAILURE
    })
}

/// Do the actual work.
///
/// Returns the exit code to use.
///
/// # Errors
///
/// This returns any errors encountered during the run so that they can be
/// outputted nicely in [`main()`].
fn cli(params: &Params) -> anyhow::Result<ExitCode> {
    logging::init(params.verbose)?;

    match &params.command {
        Command::CheckConfig { path } => check_config(params, path),
    }
}

/// Implement `check-config`.
fn check_config(params: &Params, path: &Path) -> anyhow::Result<ExitCode> {
    if path.is_absolute() {
        bail!("path must be relative to the repository root");
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let config_dir = path.parent().unwrap_or_else(|| Path::new(""));
    let report = devcontainer::check(&devcontainer::parse(&text)?, config_dir);

    let mut out = params.out_stream();
    for property in &report.ignored {
        writeln!(out, "ignored: {property}")?;
    }
    for violation in &report.violations {
        writeln!(
            out,
            "rejected: {}: {}",
            violation.property, violation.reason
        )?;
    }
    if report.is_ok() {
        writeln!(out, "ok")?;
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}
