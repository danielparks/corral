//! corral executable.

use std::io::Write;
use std::process::ExitCode;

mod client;
mod config;
mod devcontainer;
mod files;
mod git;
mod layout;
mod logging;
mod params;
mod runs;
mod shim;

use anyhow::{Context, bail};
use layout::Layout;
use params::{Command, HostCommand, Params, Parser, SecretCommand};
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
        Command::PodmanShim { args } => {
            shim::exec_podman(args).map(|()| ExitCode::SUCCESS)
        }
        Command::Host { command } => host(params, command),
        Command::Init { name, address, user, no_passphrase } => {
            client::init(
                name,
                address,
                user,
                *no_passphrase,
                &mut params.out_stream(),
            )?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Exec { name, command } => client(params)?.exec(name, command),
        Command::New => client(params)?.new_project().map(ok),
        Command::Up { name, base } => client(params)?.up(name, base).map(ok),
        Command::Ls => client(params)?.ls().map(ok),
        Command::Pull { name } => {
            client(params)?.pull(name, &mut params.out_stream()).map(ok)
        }
        Command::Put { name, paths } => {
            client(params)?.put(name, paths).map(ok)
        }
        Command::Secret { command } => {
            let client = client(params)?;
            match command {
                SecretCommand::Set { name } => client.secret_set(name),
                SecretCommand::Ls => client.secret_ls(),
                SecretCommand::Rm { name } => client.secret_rm(name),
            }
            .map(ok)
        }
        Command::Rm { name } => client(params)?.rm(name).map(ok),
    }
}

/// Map a successful result to a successful exit code.
const fn ok((): ()) -> ExitCode {
    ExitCode::SUCCESS
}

/// Build the client from the command line and config.
fn client(params: &Params) -> anyhow::Result<client::Client> {
    client::Client::new(params.host.as_deref(), params.project_name.as_deref())
}

/// Run a command that operates on this machine as the host.
fn host(params: &Params, command: &HostCommand) -> anyhow::Result<ExitCode> {
    let layout = Layout::from_env()?;
    match command {
        HostCommand::New { project } => {
            runs::new_project(&layout, project)?;
        }
        HostCommand::Path { project } => {
            writeln!(
                params.out_stream(),
                "{}",
                layout.repo(project)?.display()
            )?;
        }
        HostCommand::Up { project, name, base } => {
            runs::up(&layout, project, name, base)?;
        }
        HostCommand::Ls => runs::list(&layout, &mut params.out_stream())?,
        HostCommand::Exec { project, name, command } => {
            runs::exec(&layout, project, name, command)?;
        }
        HostCommand::Sync { project, name } => {
            runs::sync(&layout, project, name, &mut params.out_stream())?;
        }
        HostCommand::Rm { project, name } => runs::rm(&layout, project, name)?,
        HostCommand::Put { project, name } => {
            files::put(&layout, project, name, std::io::stdin().lock())?;
        }
        HostCommand::SecretSet { project, name } => {
            files::secret_set(&layout, project, name, std::io::stdin().lock())?;
        }
        HostCommand::SecretLs { project } => {
            files::secret_list(&layout, project, &mut params.out_stream())?;
        }
        HostCommand::SecretRm { project, name } => {
            files::secret_remove(&layout, project, name)?;
        }
    }
    Ok(ExitCode::SUCCESS)
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
