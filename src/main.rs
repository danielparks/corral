//! corral executable.

use std::io::Write;
use std::process::ExitCode;

mod devcontainer;
mod git;
mod layout;
mod logging;
mod params;
mod runs;
mod shim;

use anyhow::{Context, bail};
use layout::Layout;
use params::{Command, Params, Parser};
use std::path::Path;

/// Name this executable is symlinked as to act as the podman filter.
pub const SHIM_NAME: &str = "corral-podman";

/// Wrapper to handle errors.
///
/// See [`cli()`].
fn main() -> ExitCode {
    if invoked_as_shim() {
        return run_shim();
    }

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
        Command::New { project } => {
            runs::new_project(&Layout::from_env()?, project)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Up { project, name, base } => {
            runs::up(&Layout::from_env()?, project, name, base)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Ls => {
            runs::list(&Layout::from_env()?, &mut params.out_stream())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Exec { project, name, command } => {
            runs::exec(&Layout::from_env()?, project, name, command)
                .map(|()| ExitCode::SUCCESS)
        }
        Command::Sync { project, name } => {
            runs::sync(
                &Layout::from_env()?,
                project,
                name,
                &mut params.out_stream(),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Rm { project, name } => {
            runs::rm(&Layout::from_env()?, project, name)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::PodmanShim { args } => {
            shim::exec_podman(args).map(|()| ExitCode::SUCCESS)
        }
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

/// Whether this executable was started under the shim's name.
fn invoked_as_shim() -> bool {
    std::env::args_os()
        .next()
        .and_then(|arg| Path::new(&arg).file_name().map(|n| n == SHIM_NAME))
        .unwrap_or(false)
}

/// Act as `podman`, but only for arguments the policy allows.
fn run_shim() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    // Only returns if podman could not be started or the arguments were
    // refused.
    if let Err(error) = shim::exec_podman(&args) {
        eprintln!("corral: {error:#}");
    }
    ExitCode::FAILURE
}
