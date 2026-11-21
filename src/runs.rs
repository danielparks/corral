//! Creating, inspecting, syncing, and removing runs.
//!
//! The workspace clone is untrusted: the agent can write anything in it,
//! including `.git/config` and hooks. So nothing here ever runs git inside it.
//! Config and builds come from `pinned/`, an export of the base commit, and
//! results are brought back with commands run against the trusted bare repo.

use crate::git::{self, bare, git};
use crate::layout::{Layout, Run};
use crate::{devcontainer, shim};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where to look for the devcontainer config in a repository.
const CONFIG_PATHS: &[&str] =
    &[".devcontainer/devcontainer.json", ".devcontainer.json"];

/// Patterns that are never included in snapshots of uncommitted work.
const EXCLUDES: &str = "\
node_modules/
target/
.venv/
__pycache__/
.env
.env.*
*.pem
*.key
id_rsa*
id_ed25519*
.DS_Store
";

/// Label key the run's container is tagged with.
const LABEL: &str = "corral.run";

/// What is remembered about a run.
#[derive(Debug, Deserialize, Serialize)]
pub struct Meta {
    /// Branch or commit the run started from.
    pub base: String,

    /// Commit that `base` resolved to.
    pub commit: String,

    /// Branch the agent works on.
    pub branch: String,

    /// devcontainer config path, relative to the repo and to `pinned/`.
    pub config: String,

    /// Container user.
    pub remote_user: String,

    /// Workspace path inside the container.
    pub remote_workspace: String,
}

/// Create a bare repository for `project`.
///
/// # Errors
///
/// Returns an error if it exists or cannot be created.
pub fn new_project(layout: &Layout, project: &str) -> Result<()> {
    let repo = layout.repo(project)?;
    if repo.exists() {
        bail!("project already exists: {}", repo.display());
    }
    std::fs::create_dir_all(layout.root())?;
    git::run(
        git()
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&repo),
    )?;
    // Only fast-forwards, and no deleting: the laptop is the source of truth.
    git::run(bare(&repo).args(["config", "receive.denyDeletes", "true"]))?;
    git::run(bare(&repo).args([
        "config",
        "receive.denyNonFastForwards",
        "true",
    ]))?;
    Ok(())
}

/// Start a run: pin the config, clone the workspace, and bring up the
/// container.
///
/// # Errors
///
/// Returns an error if the run exists, the config is not acceptable, or any
/// step fails. A failed start is cleaned up.
pub fn up(
    layout: &Layout,
    project: &str,
    name: &str,
    base: &str,
) -> Result<()> {
    let run = layout.run(project, name)?;
    if run.dir.exists() {
        bail!("run already exists: {}", run.label());
    }
    if !run.repo.exists() {
        bail!("no such project: {project}");
    }

    std::fs::create_dir_all(run.tmp())?;
    let result = up_inner(layout, &run, base);
    if result.is_err()
        && let Err(error) = remove(&run)
    {
        eprintln!("warning: cleanup failed: {error:#}");
    }
    result
}

/// Do the work of [`up()`].
fn up_inner(layout: &Layout, run: &Run, base: &str) -> Result<()> {
    let commit = git::output(bare(&run.repo).args([
        "rev-parse",
        "--verify",
        &format!("{base}^{{commit}}"),
    ]))
    .with_context(|| format!("no such branch or commit: {base}"))?;

    // Config comes from the base commit, never from the workspace.
    let (config, text) = read_config(&run.repo, &commit)?;
    let config_dir =
        Path::new(&config).parent().unwrap_or_else(|| Path::new(""));
    let report = devcontainer::check(&devcontainer::parse(&text)?, config_dir);
    if !report.is_ok() {
        let problems: Vec<String> = report
            .violations
            .iter()
            .map(|v| format!("  {}: {}", v.property, v.reason))
            .collect();
        bail!("{config} is not allowed:\n{}", problems.join("\n"));
    }

    export_commit(&run.repo, &commit, &run.pinned())?;

    // Not hardlinked: the container must not be able to alter objects that
    // the bare repository shares.
    let workspace = run.workspace();
    git::run(
        git()
            .args(["clone", "-q", "--no-hardlinks"])
            .arg(&run.repo)
            .arg(&workspace),
    )?;
    let in_workspace =
        |args: &[&str]| git::run(git().arg("-C").arg(&workspace).args(args));
    in_workspace(&["checkout", "-q", "-b", &run.branch(), &commit])?;
    in_workspace(&["remote", "set-url", "origin", shim::ORIGIN_TARGET])?;
    in_workspace(&["config", "user.name", "corral agent"])?;
    in_workspace(&["config", "user.email", "agent@corral.invalid"])?;

    let mut up = devcontainer_command(layout, run, &config, "up")?;
    up.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let out = up.output().context("could not run devcontainer")?;
    if !out.status.success() {
        bail!("devcontainer up failed ({})", out.status);
    }
    let result = parse_up_result(&String::from_utf8_lossy(&out.stdout))?;

    let meta = Meta {
        base: base.to_owned(),
        commit,
        branch: run.branch(),
        config,
        remote_user: result.0,
        remote_workspace: result.1,
    };
    std::fs::write(run.meta(), serde_json::to_string_pretty(&meta)?)?;
    Ok(())
}

/// Find and read the devcontainer config in `commit`.
fn read_config(repo: &Path, commit: &str) -> Result<(String, String)> {
    for path in CONFIG_PATHS {
        let spec = format!("{commit}:{path}");
        if let Ok(text) = git::output(bare(repo).args(["show", &spec])) {
            return Ok(((*path).to_owned(), text));
        }
    }
    bail!("no devcontainer.json in {commit} (looked for {CONFIG_PATHS:?})")
}

/// Export the files in `commit` into `dest`.
fn export_commit(repo: &Path, commit: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    let mut archive = bare(repo)
        .args(["archive", "--format=tar", commit])
        .stdout(Stdio::piped())
        .spawn()
        .context("could not run git archive")?;
    let status = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(dest)
        .stdin(archive.stdout.take().context("no archive output")?)
        .status()
        .context("could not run tar")?;
    if !archive.wait()?.success() || !status.success() {
        bail!("could not export {commit}");
    }
    Ok(())
}

/// Get the user and workspace folder from the JSON line `up` prints.
fn parse_up_result(stdout: &str) -> Result<(String, String)> {
    let line = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .context("devcontainer up printed no result")?;
    let result: serde_json::Value = serde_json::from_str(line)?;
    if result["outcome"] != "success" {
        bail!("devcontainer up failed: {line}");
    }
    let field = |name: &str| {
        result[name]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("result has no {name}"))
    };
    Ok((field("remoteUser")?, field("remoteWorkspaceFolder")?))
}

/// Build a `devcontainer <subcommand>` command for a run.
///
/// The CLI is pointed at the pinned config and at the filtering shim, and its
/// scratch space is inside the run.
fn devcontainer_command(
    layout: &Layout,
    run: &Run,
    config: &str,
    subcommand: &str,
) -> Result<Command> {
    let shim = ensure_shim(layout)?;
    let roots: Vec<OsString> = vec![
        run.workspace().into(),
        run.pinned().into(),
        run.tmp().into(),
    ];
    let roots = std::env::join_paths(roots)?;

    let program = std::env::var_os("CORRAL_DEVCONTAINER")
        .unwrap_or_else(|| "devcontainer".into());
    let mut command = Command::new(program);
    command
        .arg(subcommand)
        .arg("--workspace-folder")
        .arg(run.workspace())
        .arg("--config")
        .arg(run.pinned().join(config))
        .arg("--docker-path")
        .arg(shim)
        .arg("--id-label")
        .arg(format!("{LABEL}={}", run.label()))
        .env("CORRAL_BIND_ROOTS", roots)
        .env("CORRAL_ORIGIN", &run.repo)
        .env("TMPDIR", run.tmp());
    Ok(command)
}

/// Make sure `bin/corral-podman` points at this executable, and return it.
///
/// The shim decides what it is by its name; see `main()`.
fn ensure_shim(layout: &Layout) -> Result<std::path::PathBuf> {
    let dir = layout.bin_dir();
    std::fs::create_dir_all(&dir)?;
    let link = dir.join(crate::SHIM_NAME);
    let exe = std::env::current_exe()?;
    if std::fs::read_link(&link).ok().as_deref() != Some(exe.as_path()) {
        match std::fs::remove_file(&link) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        std::os::unix::fs::symlink(&exe, &link)?;
    }
    Ok(link)
}

/// Load a run's metadata.
fn load(layout: &Layout, project: &str, name: &str) -> Result<(Run, Meta)> {
    let run = layout.run(project, name)?;
    let text = std::fs::read_to_string(run.meta())
        .with_context(|| format!("no such run: {}", run.label()))?;
    Ok((run, serde_json::from_str(&text)?))
}

/// Run a command in a run's container, replacing this process.
///
/// # Errors
///
/// Returns an error if the run does not exist or the command cannot start.
pub fn exec(
    layout: &Layout,
    project: &str,
    name: &str,
    command: &[String],
) -> Result<()> {
    let (run, meta) = load(layout, project, name)?;
    let mut exec = devcontainer_command(layout, &run, &meta.config, "exec")?;
    let error = exec.args(command).exec();
    Err(error).context("could not run devcontainer")
}

/// List runs with their container state.
///
/// # Errors
///
/// Returns an error if the runs directory cannot be read.
pub fn list(layout: &Layout, out: &mut dyn Write) -> Result<()> {
    let states = container_states();
    let Ok(projects) = std::fs::read_dir(layout.runs_dir()) else {
        return Ok(());
    };
    let mut rows = Vec::new();
    for project in projects.flatten() {
        for run in std::fs::read_dir(project.path())?.flatten() {
            let (project, name) = (
                project.file_name().to_string_lossy().into_owned(),
                run.file_name().to_string_lossy().into_owned(),
            );
            let Ok((run, meta)) = load(layout, &project, &name) else {
                continue;
            };
            let state = states
                .get(&run.label())
                .map_or("no container", String::as_str);
            rows.push(format!(
                "{}\t{state}\t{}\t{}",
                run.label(),
                meta.branch,
                meta.base
            ));
        }
    }
    rows.sort();
    for row in rows {
        writeln!(out, "{row}")?;
    }
    Ok(())
}

/// Map run labels to container status text.
fn container_states() -> BTreeMap<String, String> {
    let format = format!("{{{{index .Labels \"{LABEL}\"}}}}\t{{{{.Status}}}}");
    let mut command = Command::new(podman());
    command
        .args(["ps", "-a", "--filter", &format!("label={LABEL}")])
        .args(["--format", &format]);
    let Ok(text) = git::output(&mut command) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(label, status)| (label.to_owned(), status.to_owned()))
        .collect()
}

/// The `podman` to use for our own (trusted) queries.
fn podman() -> OsString {
    std::env::var_os("CORRAL_PODMAN").unwrap_or_else(|| "podman".into())
}

/// Bring the agent's work into the bare repository.
///
/// Fetches the run's branch, and records any uncommitted changes as a commit
/// on a separate snapshot ref. Both are done by git running against the
/// trusted repository, which only reads files from the untrusted workspace.
///
/// # Errors
///
/// Returns an error if the run does not exist or git fails.
pub fn sync(
    layout: &Layout,
    project: &str,
    name: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let (run, _) = load(layout, project, name)?;
    let branch = run.branch();

    git::run(
        bare(&run.repo)
            .arg("fetch")
            .arg("-q")
            .arg(run.workspace())
            .arg(format!("+refs/heads/{branch}:refs/heads/{branch}")),
    )
    .with_context(|| format!("could not fetch {branch} from the workspace"))?;
    let tip = git::output(bare(&run.repo).args([
        "rev-parse",
        "--verify",
        &format!("refs/heads/{branch}"),
    ]))?;
    writeln!(out, "{branch} is at {}", short(&tip))?;

    match snapshot(layout, &run, &tip)? {
        Some(commit) => writeln!(
            out,
            "uncommitted changes saved as {} ({})",
            run.snapshot_ref(),
            short(&commit)
        )?,
        None => writeln!(out, "no uncommitted changes")?,
    }
    Ok(())
}

/// First characters of a commit id.
fn short(commit: &str) -> &str {
    commit.get(..10).unwrap_or(commit)
}

/// Record the workspace's files, if they differ from `tip`, on the run's
/// snapshot ref. Returns the new commit.
fn snapshot(layout: &Layout, run: &Run, tip: &str) -> Result<Option<String>> {
    let excludes = layout.excludes();
    std::fs::write(&excludes, EXCLUDES)?;
    let index = run.dir.join("snapshot.index");
    let _ = std::fs::remove_file(&index);

    // Explicit git dir and work tree, so nothing is read from the
    // workspace's own `.git`.
    let work = |args: &[&str]| -> Result<String> {
        let mut command = bare(&run.repo);
        command
            .env("GIT_INDEX_FILE", &index)
            .env("GIT_WORK_TREE", run.workspace())
            .arg("-c")
            .arg(format!("core.excludesFile={}", excludes.display()))
            .args(args);
        git::output(&mut command)
    };
    work(&["read-tree", tip])?;
    work(&["add", "-A", "--", "."])?;
    let tree = work(&["write-tree"])?;
    let _ = std::fs::remove_file(&index);

    let tip_tree = git::output(
        bare(&run.repo).args(["rev-parse", &format!("{tip}^{{tree}}")]),
    )?;
    if tree == tip_tree {
        return Ok(None);
    }

    let commit = git::output(
        bare(&run.repo)
            .args(["-c", "user.name=corral", "-c", "user.email=corral@invalid"])
            .args(["commit-tree", &tree, "-p", tip, "-m"])
            .arg(format!("Snapshot of uncommitted work in {}", run.label())),
    )?;
    git::run(bare(&run.repo).args([
        "update-ref",
        &run.snapshot_ref(),
        &commit,
    ]))?;
    Ok(Some(commit))
}

/// Remove a run's container and files.
///
/// # Errors
///
/// Returns an error if the run does not exist or cannot be removed.
pub fn rm(layout: &Layout, project: &str, name: &str) -> Result<()> {
    let run = layout.run(project, name)?;
    if !run.dir.exists() {
        bail!("no such run: {}", run.label());
    }
    remove(&run)
}

/// Remove the container and directory of `run`, if they exist.
fn remove(run: &Run) -> Result<()> {
    let mut list = Command::new(podman());
    list.args(["ps", "-aq", "--filter"])
        .arg(format!("label={LABEL}={}", run.label()));
    let ids = git::output(&mut list)?;
    for id in ids.split_whitespace() {
        git::run(Command::new(podman()).args(["rm", "-f", id]))?;
    }

    // Files created by root inside the container belong to a subuid that
    // only `podman unshare` can remove.
    git::run(
        Command::new(podman())
            .args(["unshare", "rm", "-rf"])
            .arg(&run.dir),
    )
}
