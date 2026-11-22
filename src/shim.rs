//! Filter for `podman` invocations made by the devcontainer CLI.
//!
//! The CLI is pointed at this instead of `podman` (via `--docker-path`).
//! Whatever the `devcontainer.json` or its features ask for, the container
//! engine only sees arguments that pass this allowlist. In particular,
//! features can add privileges through image metadata, which a check of the
//! config file alone would miss.
//!
//! Unknown subcommands and options are refused, so new CLI behavior fails
//! closed.

use anyhow::{Context, Result, bail};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Where the read-only origin repository appears inside containers.
pub const ORIGIN_TARGET: &str = "/srv/origin.git";

/// Where files sent with `put` appear inside containers.
pub const INBOX_TARGET: &str = "/corral/inbox";

/// Where the project's secrets appear inside containers: one file each.
pub const SECRETS_TARGET: &str = "/run/corral/secrets";

/// What a run is allowed to touch.
#[derive(Debug, Default)]
pub struct Policy {
    /// Host directories that bind mounts and build inputs may come from.
    pub bind_roots: Vec<PathBuf>,

    /// Host paths to mount read-only in every container, with where they
    /// appear inside it.
    pub ro_mounts: Vec<(PathBuf, String)>,
}

impl Policy {
    /// Read the policy from `CORRAL_BIND_ROOTS` (colon-separated) and
    /// `CORRAL_RO_MOUNTS` (a JSON array of `[source, target]` pairs).
    ///
    /// # Errors
    ///
    /// Refuses to run without any bind roots or with a relative path, so a
    /// missing configuration cannot widen access.
    pub fn from_env() -> Result<Self> {
        let roots = std::env::var("CORRAL_BIND_ROOTS")
            .context("CORRAL_BIND_ROOTS is not set")?;
        let bind_roots: Vec<PathBuf> = roots
            .split(':')
            .filter(|root| !root.is_empty())
            .map(PathBuf::from)
            .collect();
        if bind_roots.is_empty() || bind_roots.iter().any(|r| !r.is_absolute())
        {
            bail!("CORRAL_BIND_ROOTS must list absolute paths");
        }
        let ro_mounts = match std::env::var("CORRAL_RO_MOUNTS") {
            Ok(json) => parse_ro_mounts(&json)?,
            Err(_) => Vec::new(),
        };
        Ok(Self { bind_roots, ro_mounts })
    }

    /// Whether `path` is absolute and inside one of the bind roots.
    fn allows_path(&self, path: &str) -> bool {
        let path = Path::new(path);
        path.is_absolute()
            && !path.components().any(|c| c == Component::ParentDir)
            && self.bind_roots.iter().any(|root| path.starts_with(root))
    }
}

/// Parse `CORRAL_RO_MOUNTS`.
fn parse_ro_mounts(json: &str) -> Result<Vec<(PathBuf, String)>> {
    let pairs: Vec<(String, String)> =
        serde_json::from_str(json).context("invalid CORRAL_RO_MOUNTS")?;
    for (source, target) in &pairs {
        if !Path::new(source).is_absolute() || !target.starts_with('/') {
            bail!("CORRAL_RO_MOUNTS paths must be absolute: {source} {target}");
        }
        if source.contains(',') || target.contains(',') {
            bail!("CORRAL_RO_MOUNTS paths may not contain commas");
        }
    }
    Ok(pairs
        .into_iter()
        .map(|(source, target)| (PathBuf::from(source), target))
        .collect())
}

/// What the value of an option must satisfy.
#[derive(Clone, Copy)]
enum Rule {
    /// Anything.
    Any,
    /// One of these exact strings.
    OneOf(&'static [&'static str]),
    /// A `--mount` specification.
    Mount,
    /// A `--volume` specification.
    Volume,
    /// A host path inside the bind roots.
    Path,
    /// `name=path` for `--build-context`, with the path inside the roots.
    BuildContext,
}

/// Whether an option takes a value.
#[derive(Clone, Copy)]
enum Kind {
    /// A boolean flag: `--flag`, `--flag=true`, or `--flag=false`.
    Bool,
    /// An option whose value is checked.
    Value(Rule),
}

/// An allowed option.
struct Opt {
    /// All spellings.
    names: &'static [&'static str],

    /// What it accepts.
    kind: Kind,
}

/// Shorthand for building the option tables.
const fn opt(names: &'static [&'static str], kind: Kind) -> Opt {
    Opt { names, kind }
}

/// Options allowed for `podman run`.
const RUN_OPTS: &[Opt] = &[
    opt(
        &["-a", "--attach"],
        Kind::Value(Rule::OneOf(&["STDIN", "STDOUT", "STDERR"])),
    ),
    opt(&["-d", "--detach"], Kind::Bool),
    opt(&["--entrypoint"], Kind::Value(Rule::Any)),
    opt(&["-e", "--env"], Kind::Value(Rule::Any)),
    opt(&["--hostname"], Kind::Value(Rule::Any)),
    opt(&["-i", "--interactive"], Kind::Bool),
    opt(&["--init"], Kind::Bool),
    opt(&["-l", "--label"], Kind::Value(Rule::Any)),
    opt(&["--mount"], Kind::Value(Rule::Mount)),
    opt(&["--name"], Kind::Value(Rule::Any)),
    opt(&["--rm"], Kind::Bool),
    opt(
        &["--security-opt"],
        Kind::Value(Rule::OneOf(&["label=disable"])),
    ),
    opt(&["--sig-proxy"], Kind::Bool),
    opt(&["-t", "--tty"], Kind::Bool),
    opt(&["-u", "--user"], Kind::Value(Rule::Any)),
    opt(&["--userns"], Kind::Value(Rule::OneOf(&["keep-id"]))),
    opt(&["-v", "--volume"], Kind::Value(Rule::Volume)),
    opt(&["-w", "--workdir"], Kind::Value(Rule::Any)),
];

/// Options allowed for `podman exec`.
const EXEC_OPTS: &[Opt] = &[
    opt(&["-e", "--env"], Kind::Value(Rule::Any)),
    opt(&["-i", "--interactive"], Kind::Bool),
    opt(&["-t", "--tty"], Kind::Bool),
    opt(&["-u", "--user"], Kind::Value(Rule::Any)),
    opt(&["-w", "--workdir"], Kind::Value(Rule::Any)),
];

/// Options allowed for `podman build` and `podman buildx build`.
const BUILD_OPTS: &[Opt] = &[
    opt(&["--build-arg"], Kind::Value(Rule::Any)),
    opt(&["--build-context"], Kind::Value(Rule::BuildContext)),
    opt(&["-f", "--file"], Kind::Value(Rule::Path)),
    opt(&["--label"], Kind::Value(Rule::Any)),
    opt(&["--load"], Kind::Bool),
    opt(&["--no-cache"], Kind::Bool),
    opt(&["--platform"], Kind::Value(Rule::Any)),
    opt(&["--pull"], Kind::Bool),
    opt(&["--target"], Kind::Value(Rule::Any)),
    opt(&["-t", "--tag"], Kind::Value(Rule::Any)),
];

/// Replace `podman` with the filtered invocation of it.
///
/// Reads the policy from the environment, checks `args`, and executes the
/// real `podman` (`CORRAL_PODMAN`, default `podman`). Only returns on error.
///
/// # Errors
///
/// Returns an error if `args` are refused or `podman` cannot be executed.
pub fn exec_podman(args: &[String]) -> Result<()> {
    let policy = Policy::from_env()?;
    let args = filter(&policy, args)
        .context("refusing to run podman with these arguments")?;
    let podman =
        std::env::var_os("CORRAL_PODMAN").unwrap_or_else(|| "podman".into());
    let error = Command::new(&podman).args(args).exec();
    Err(error).with_context(|| format!("could not run {podman:?}"))
}

/// Check `args` for `podman` and return the arguments to actually use.
///
/// # Errors
///
/// Returns an error describing the first thing that is not allowed.
pub fn filter(policy: &Policy, args: &[String]) -> Result<Vec<String>> {
    let Some((command, rest)) = args.split_first() else {
        bail!("no subcommand");
    };

    match (command.as_str(), rest.first().map(String::as_str)) {
        // Read-only queries and container housekeeping.
        (
            "-v" | "--version" | "version" | "info" | "ps" | "inspect"
            | "events" | "pull" | "start" | "stop" | "rm",
            _,
        )
        | ("buildx", Some("version")) => Ok(args.to_vec()),
        ("run", _) => run(policy, rest),
        ("exec", _) => exec(policy, rest),
        ("build", _) => Ok(prefixed(&["build"], build(policy, rest)?)),
        ("buildx", Some("build")) => {
            Ok(prefixed(&["buildx", "build"], build(policy, &rest[1..])?))
        }
        _ => bail!("subcommand not allowed: {command}"),
    }
}

/// Put `prefix` in front of `args`.
fn prefixed(prefix: &[&str], args: Vec<String>) -> Vec<String> {
    prefix.iter().map(|s| (*s).to_owned()).chain(args).collect()
}

/// Filter `podman run`.
fn run(policy: &Policy, args: &[String]) -> Result<Vec<String>> {
    let (options, rest) = parse_options(policy, RUN_OPTS, args)?;
    if rest.is_empty() {
        bail!("run: no image");
    }

    let mut out = vec!["run".to_owned()];
    for (source, target) in &policy.ro_mounts {
        out.push("--mount".to_owned());
        out.push(format!(
            "type=bind,source={},target={target},readonly",
            source.display()
        ));
    }
    out.extend(options);
    out.extend(rest.iter().cloned());
    Ok(out)
}

/// Filter `podman exec`.
fn exec(policy: &Policy, args: &[String]) -> Result<Vec<String>> {
    let (options, rest) = parse_options(policy, EXEC_OPTS, args)?;
    if rest.is_empty() {
        bail!("exec: no container");
    }

    let mut out = vec!["exec".to_owned()];
    out.extend(options);
    out.extend(rest.iter().cloned());
    Ok(out)
}

/// Filter the arguments after `podman build` (or `buildx build`).
fn build(policy: &Policy, args: &[String]) -> Result<Vec<String>> {
    let (mut options, rest) = parse_options(policy, BUILD_OPTS, args)?;
    match rest {
        [context] if context == "-" || policy.allows_path(context) => {
            options.push(context.clone());
            Ok(options)
        }
        [context] => bail!("build context not allowed: {context}"),
        _ => bail!("build: expected exactly one context"),
    }
}

/// Validate leading options, returning them and the remaining arguments.
fn parse_options<'a>(
    policy: &Policy,
    table: &[Opt],
    mut args: &'a [String],
) -> Result<(Vec<String>, &'a [String])> {
    let mut out = Vec::new();

    while let Some((arg, tail)) = args.split_first() {
        if arg == "-" || !arg.starts_with('-') {
            break;
        }

        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name, Some(value))
            }
            _ => (arg.as_str(), None),
        };
        let option = table
            .iter()
            .find(|o| o.names.contains(&name))
            .with_context(|| format!("option not allowed: {arg}"))?;

        match (option.kind, inline) {
            (Kind::Bool, None | Some("true" | "false")) => {
                out.push(arg.clone());
                args = tail;
            }
            (Kind::Bool, Some(_)) => bail!("bad value for {name}"),
            (Kind::Value(rule), Some(value)) => {
                check(policy, rule, value)
                    .with_context(|| format!("{name} value not allowed"))?;
                out.push(arg.clone());
                args = tail;
            }
            (Kind::Value(rule), None) => {
                let Some((value, tail)) = tail.split_first() else {
                    bail!("{name} needs a value");
                };
                check(policy, rule, value)
                    .with_context(|| format!("{name} value not allowed"))?;
                out.push(arg.clone());
                out.push(value.clone());
                args = tail;
            }
        }
    }

    Ok((out, args))
}

/// Check one option value against its rule.
fn check(policy: &Policy, rule: Rule, value: &str) -> Result<()> {
    match rule {
        Rule::Any => Ok(()),
        Rule::OneOf(allowed) if allowed.contains(&value) => Ok(()),
        Rule::OneOf(_) => bail!("{value:?}"),
        Rule::Mount => check_mount(policy, value),
        Rule::Volume => check_volume(policy, value),
        Rule::Path if policy.allows_path(value) => Ok(()),
        Rule::Path => bail!("path outside allowed roots: {value}"),
        Rule::BuildContext => match value.split_once('=') {
            Some((_, source))
                if source.starts_with("docker-image://")
                    || policy.allows_path(source) =>
            {
                Ok(())
            }
            _ => bail!("build context outside allowed roots: {value}"),
        },
    }
}

/// Check a `--mount type=...,source=...,target=...` specification.
fn check_mount(policy: &Policy, spec: &str) -> Result<()> {
    let mut kind = "volume";
    let mut source = None;
    let mut keys = Vec::new();
    for field in spec.split(',') {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        match key {
            "type" => kind = value,
            "source" | "src" => source = Some(value),
            _ => {}
        }
        keys.push(key);
    }

    let allowed: &[&str] = match kind {
        "bind" => &[
            "type",
            "source",
            "src",
            "target",
            "dst",
            "destination",
            "readonly",
            "ro",
            "consistency",
        ],
        "volume" => &[
            "type",
            "source",
            "src",
            "target",
            "dst",
            "destination",
            "readonly",
            "ro",
        ],
        "tmpfs" => &["type", "target", "dst", "destination", "tmpfs-size"],
        _ => bail!("mount type not allowed: {kind}"),
    };
    if let Some(key) = keys.iter().find(|k| !allowed.contains(k)) {
        bail!("mount option not allowed: {key}");
    }

    match (kind, source) {
        ("bind", Some(source)) if policy.allows_path(source) => Ok(()),
        ("bind", _) => bail!("bind mount source not allowed"),
        ("volume", Some(source)) if source.contains('/') => {
            bail!("volume name looks like a path: {source}")
        }
        _ => Ok(()),
    }
}

/// Check a `--volume source:target[:options]` specification.
fn check_volume(policy: &Policy, spec: &str) -> Result<()> {
    let mut parts = spec.split(':');
    let (Some(source), Some(_target)) = (parts.next(), parts.next()) else {
        bail!("expected source:target");
    };
    let options = parts.next().unwrap_or("");
    if parts.next().is_some() {
        bail!("too many fields");
    }
    if options
        .split(',')
        .any(|o| !["", "ro", "rw", "z", "Z"].contains(&o))
    {
        bail!("volume options not allowed: {options}");
    }

    if source.starts_with(['/', '.', '~']) {
        if policy.allows_path(source) {
            Ok(())
        } else {
            bail!("bind mount source not allowed: {source}")
        }
    } else if source.contains('/') {
        bail!("volume name looks like a path: {source}")
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A policy like the one for a real run.
    fn policy() -> Policy {
        Policy {
            bind_roots: vec!["/runs/app/x".into(), "/runs/app/x.tmp".into()],
            ro_mounts: vec![(
                "/repos/app.git".into(),
                ORIGIN_TARGET.to_owned(),
            )],
        }
    }

    /// Split a command line on whitespace.
    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    /// Why `line` is refused, or panic if it is not.
    fn refused(line: &str) -> String {
        let error = filter(&policy(), &args(line)).unwrap_err();
        format!("{error:#}")
    }

    #[test]
    fn real_run_from_devcontainer_cli() {
        let line = "run --sig-proxy=false -a STDOUT -a STDERR \
            --mount type=bind,source=/runs/app/x,target=/workspaces/x \
            -l devcontainer.local_folder=/runs/app/x \
            --security-opt label=disable --userns=keep-id \
            --entrypoint /bin/sh -l a=b image-id -c echo-hi -";
        let out = filter(&policy(), &args(line)).unwrap();
        assert_eq!(
            out[..3],
            [
                "run",
                "--mount",
                "type=bind,source=/repos/app.git,target=/srv/origin.git,\
                 readonly"
            ]
        );
        assert_eq!(out[3..], args(line)[1..]);
    }

    #[test]
    fn real_build_from_devcontainer_cli() {
        let line = "buildx build --load --build-arg BUILDKIT_INLINE_CACHE=1 \
            -f /runs/app/x.tmp/features/Dockerfile -t vsc-x \
            --target dev_containers_target_stage --build-arg A=b \
            --build-context dev_containers_feature_content_source=\
            /runs/app/x.tmp/features /runs/app/x";
        assert_eq!(filter(&policy(), &args(line)).unwrap(), args(line));
    }

    #[test]
    fn real_update_uid_build_reads_stdin() {
        let line = "build -f /runs/app/x.tmp/u.Dockerfile -t t \
            --platform linux/amd64 --build-arg NEW_UID=1100 -";
        assert_eq!(filter(&policy(), &args(line)).unwrap(), args(line));
    }

    #[test]
    fn queries_pass_through() {
        for line in [
            "version --format {{.Server.Version}}",
            "ps -q -a --filter label=x=y",
            "inspect --type image foo",
            "events --format json --filter event=start",
            "buildx version",
            "-v",
        ] {
            assert_eq!(filter(&policy(), &args(line)).unwrap(), args(line));
        }
    }

    #[test]
    fn exec_allows_user_and_env() {
        let line = "exec -i -u node -e A=b abc123 /bin/sh -c ls";
        assert_eq!(filter(&policy(), &args(line)).unwrap(), args(line));
    }

    #[test]
    fn privilege_options_are_refused() {
        for line in [
            "run --privileged img",
            "run --cap-add=ALL img",
            "run --cap-add ALL img",
            "run --security-opt seccomp=unconfined img",
            "run --device /dev/fuse img",
            "run --network=host img",
            "run --pid=host img",
            "run --userns=host img",
            "run -p 80:80 img",
            "run --group-add keep-groups img",
            "exec --privileged abc sh",
        ] {
            assert!(
                filter(&policy(), &args(line)).is_err(),
                "should refuse: {line}"
            );
        }
    }

    #[test]
    fn host_paths_outside_roots_are_refused() {
        for line in [
            "run --mount type=bind,source=/etc,target=/x img",
            "run --mount type=bind,source=/runs/app/x/../../..,target=/x img",
            "run --mount type=bind,target=/x img",
            "run --mount type=bind,source=/runs/app/xx,target=/x img",
            "run -v /etc:/x img",
            "run -v /runs/app/x:/x:shared img",
            "run -v ~:/x img",
            "run --mount type=devpts,target=/x img",
            "run --mount type=bind,source=/runs/app/x,target=/x,\
             bind-propagation=rshared img",
            "build -f /etc/Dockerfile /runs/app/x",
            "build /",
            "build --build-context a=/etc /runs/app/x",
            "build --network=host /runs/app/x",
            "build --secret id=x,src=/etc/shadow /runs/app/x",
        ] {
            assert!(
                filter(&policy(), &args(line)).is_err(),
                "should refuse: {line}"
            );
        }
    }

    #[test]
    fn volumes_and_tmpfs_are_allowed() {
        for line in [
            "run --mount type=volume,source=cache,target=/c img",
            "run --mount type=tmpfs,target=/tmp img",
            "run -v cache:/c img",
            "run -v /runs/app/x/sub:/c:ro img",
        ] {
            assert!(
                filter(&policy(), &args(line)).is_ok(),
                "should allow: {line}"
            );
        }
    }

    #[test]
    fn unknown_subcommands_are_refused() {
        assert!(refused("system service").contains("not allowed"));
        assert!(refused("cp a b").contains("not allowed"));
        assert!(refused("machine ssh").contains("not allowed"));
        assert!(refused("").contains("no subcommand"));
    }

    #[test]
    fn command_after_image_is_not_inspected() {
        let line = "run img --privileged --mount /etc";
        filter(&policy(), &args(line)).unwrap();
    }

    #[test]
    fn ro_mounts_are_parsed_and_checked() {
        let mounts =
            parse_ro_mounts(r#"[["/a/b","/c"],["/d","/e/f"]]"#).unwrap();
        assert_eq!(mounts[1], (PathBuf::from("/d"), "/e/f".to_owned()));
        assert_eq!(parse_ro_mounts("[]").unwrap(), []);
        parse_ro_mounts(r#"[["rel","/c"]]"#).unwrap_err();
        parse_ro_mounts(r#"[["/a","c"]]"#).unwrap_err();
        parse_ro_mounts(r#"[["/a,b","/c"]]"#).unwrap_err();
        parse_ro_mounts("nonsense").unwrap_err();
    }

    #[test]
    fn missing_roots_fail_closed() {
        assert!(!Policy::default().allows_path("/anything"));
    }
}
