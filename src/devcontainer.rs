//! Policy for which `devcontainer.json` properties a run may use.
//!
//! The config is untrusted input: some properties run commands on the host or
//! raise the container's privileges. This is an allowlist, so properties added
//! to the spec later are rejected until reviewed.
//!
//! This only gives early, readable errors. Features can add privileges through
//! image metadata, so the real enforcement has to happen on the final
//! `podman run` arguments.

use anyhow::Context;
use serde_json::Value;
use std::path::{Component, Path};

/// Properties that are used as-is.
const ALLOWED: &[&str] = &[
    "$schema",
    "build",
    "containerEnv",
    "containerUser",
    "features",
    "image",
    "init",
    "name",
    "onCreateCommand",
    "overrideCommand",
    "overrideFeatureInstallOrder",
    "postAttachCommand",
    "postCreateCommand",
    "postStartCommand",
    "remoteEnv",
    "remoteUser",
    "shutdownAction",
    "updateContentCommand",
    "updateRemoteUserUID",
    "userEnvProbe",
    "waitFor",
    "workspaceFolder",
    // Deprecated top-level forms of `build.*`.
    "context",
    "dockerFile",
];

/// Properties that are harmless but meaningless here (editor and UI hints).
const IGNORED: &[&str] = &[
    "customizations",
    "forwardPorts",
    "hostRequirements",
    "otherPortsAttributes",
    "portsAttributes",
];

/// Properties that are known to be dangerous, with why.
const DENIED: &[(&str, &str)] = &[
    ("initializeCommand", "runs commands on the host"),
    ("mounts", "binds host paths into the container"),
    ("workspaceMount", "binds host paths into the container"),
    ("runArgs", "passes arbitrary flags to the container engine"),
    ("privileged", "disables container confinement"),
    ("capAdd", "adds capabilities"),
    ("securityOpt", "changes security settings"),
    (
        "dockerComposeFile",
        "defines arbitrary services and privileges",
    ),
    ("service", "belongs to compose configuration"),
    ("runServices", "belongs to compose configuration"),
    ("appPort", "publishes ports on the host"),
];

/// Properties allowed inside `build`.
const ALLOWED_BUILD: &[&str] =
    &["args", "context", "dockerfile", "dockerFile", "target"];

/// A property that is not permitted.
#[derive(Debug, Eq, PartialEq)]
pub struct Violation {
    /// Dotted path of the property.
    pub property: String,

    /// Why it was rejected.
    pub reason: String,
}

/// Result of checking a config.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Report {
    /// Properties that make the config unacceptable.
    pub violations: Vec<Violation>,

    /// Properties that were dropped as irrelevant.
    pub ignored: Vec<String>,
}

impl Report {
    /// Whether the config may be used.
    #[must_use]
    pub const fn is_ok(&self) -> bool {
        self.violations.is_empty()
    }

    /// Record a violation.
    fn reject(&mut self, property: &str, reason: &str) {
        self.violations.push(Violation {
            property: property.to_owned(),
            reason: reason.to_owned(),
        });
    }
}

/// Parse `devcontainer.json`, which allows comments and trailing commas.
///
/// # Errors
///
/// Returns an error if `text` is not valid JSON with comments.
pub fn parse(text: &str) -> anyhow::Result<Value> {
    json5::from_str(text).context("invalid devcontainer.json")
}

/// Check `config` against the policy.
///
/// `config_dir` is the directory holding the config file, relative to the
/// repository root (empty for a root-level `.devcontainer.json`). Paths in the
/// config are relative to it and must not leave the repository.
#[must_use]
pub fn check(config: &Value, config_dir: &Path) -> Report {
    let mut report = Report::default();
    let Some(object) = config.as_object() else {
        report.reject("(root)", "config must be an object");
        return report;
    };

    for (key, value) in object {
        if ALLOWED.contains(&key.as_str()) {
            if key == "build" {
                check_build(value, config_dir, &mut report);
            } else if key == "context" || key == "dockerFile" {
                check_path(key, value, config_dir, &mut report);
            }
        } else if IGNORED.contains(&key.as_str()) {
            report.ignored.push(key.clone());
        } else if let Some((_, why)) = DENIED.iter().find(|(k, _)| k == key) {
            report.reject(key, why);
        } else {
            report.reject(key, "not on the allowlist");
        }
    }

    report
}

/// Check the `build` property.
fn check_build(build: &Value, config_dir: &Path, report: &mut Report) {
    let Some(object) = build.as_object() else {
        report.reject("build", "must be an object");
        return;
    };

    for (key, value) in object {
        let property = format!("build.{key}");
        if !ALLOWED_BUILD.contains(&key.as_str()) {
            report.reject(&property, "not on the allowlist");
        } else if key != "args" && key != "target" {
            check_path(&property, value, config_dir, report);
        }
    }
}

/// Check that a path property stays inside the repository.
fn check_path(
    property: &str,
    value: &Value,
    config_dir: &Path,
    report: &mut Report,
) {
    match value.as_str() {
        Some(path) if stays_inside(config_dir, Path::new(path)) => {}
        Some(_) => report.reject(property, "path leaves the repository"),
        None => report.reject(property, "must be a string"),
    }
}

/// Whether `relative`, resolved from `base`, stays within the root.
///
/// This is lexical. Symlinks are not a concern because builds only see a
/// copy of the context directory.
fn stays_inside(base: &Path, relative: &Path) -> bool {
    let mut depth: usize = 0;
    for part in base.components().chain(relative.components()) {
        match part {
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::CurDir => {}
            Component::ParentDir => {
                let Some(up) = depth.checked_sub(1) else {
                    return false;
                };
                depth = up;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Check JSON text from a `.devcontainer/` directory.
    fn check_text(text: &str) -> Report {
        check(&parse(text).unwrap(), Path::new(".devcontainer"))
    }

    /// Property names that were rejected.
    fn rejected(report: &Report) -> Vec<&str> {
        report
            .violations
            .iter()
            .map(|v| v.property.as_str())
            .collect()
    }

    #[test]
    fn typical_config_is_ok() {
        let report = check_text(
            r#"{
                // Comments and trailing commas are allowed.
                "name": "app",
                "image": "mcr.microsoft.com/devcontainers/base:bookworm",
                "features": {"ghcr.io/devcontainers/features/rust:1": {}},
                "postCreateCommand": "cargo fetch",
                "customizations": {"vscode": {"extensions": []}},
                "forwardPorts": [3000,],
            }"#,
        );
        assert!(report.is_ok(), "{report:?}");
        assert_eq!(report.ignored.len(), 2);
    }

    #[test]
    fn dangerous_properties_are_rejected() {
        let report = check_text(
            r#"{"image": "x", "initializeCommand": "id", "privileged": true,
                "mounts": [], "runArgs": ["--network=host"]}"#,
        );
        let mut got = rejected(&report);
        got.sort_unstable();
        assert_eq!(
            got,
            ["initializeCommand", "mounts", "privileged", "runArgs"]
        );
    }

    #[test]
    fn unknown_properties_are_rejected() {
        let report = check_text(r#"{"image": "x", "somethingNew": 1}"#);
        assert_eq!(rejected(&report), ["somethingNew"]);
    }

    #[test]
    fn build_context_may_be_repo_root() {
        let report = check_text(
            r#"{"build": {"dockerfile": "Dockerfile", "context": ".."}}"#,
        );
        assert!(report.is_ok(), "{report:?}");
    }

    #[test]
    fn build_paths_may_not_leave_repo() {
        let report = check_text(
            r#"{"build": {"context": "../..", "dockerfile": "/etc/x"}}"#,
        );
        let mut got = rejected(&report);
        got.sort_unstable();
        assert_eq!(got, ["build.context", "build.dockerfile"]);
    }

    #[test]
    fn build_options_are_rejected() {
        let report =
            check_text(r#"{"build": {"options": ["--network=host"]}}"#);
        assert_eq!(rejected(&report), ["build.options"]);
    }

    #[test]
    fn root_level_config_cannot_use_parent() {
        let config = parse(r#"{"build": {"context": ".."}}"#).unwrap();
        assert!(!check(&config, Path::new("")).is_ok());
    }

    #[test]
    fn non_object_is_rejected() {
        assert!(!check(&parse("[]").unwrap(), Path::new("")).is_ok());
    }
}
