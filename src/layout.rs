//! Where things live on the host.
//!
//! ```text
//! <root>/repos/<project>.git      bare repository (trusted)
//! <root>/runs/<project>/<name>/   one run:
//!     workspace/                  clone the agent works in (untrusted)
//!     pinned/                     export of the base commit (trusted)
//!     inbox/                      files sent with `put` (read-only in the run)
//!     tmp/                        scratch space for the devcontainer CLI
//!     run.json                    metadata
//! <root>/secrets/<project>/       one file per secret (read-only in its runs)
//! <root>/bin/corral-podman        symlink to this executable
//! <root>/excludes                 patterns never snapshotted
//! ```

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

/// Root directory of everything corral keeps on the host.
#[derive(Debug)]
pub struct Layout {
    /// The root.
    root: PathBuf,
}

/// Paths for one run.
#[derive(Debug)]
pub struct Run {
    /// Project name.
    pub project: String,

    /// Run name.
    pub name: String,

    /// Directory holding everything for this run.
    pub dir: PathBuf,

    /// Project's bare repository.
    pub repo: PathBuf,
}

impl Layout {
    /// Use `$CORRAL_HOME`, or `~/corral`.
    ///
    /// # Errors
    ///
    /// Returns an error if neither `CORRAL_HOME` nor `HOME` is set.
    pub fn from_env() -> Result<Self> {
        let root = match std::env::var_os("CORRAL_HOME") {
            Some(root) => PathBuf::from(root),
            None => {
                Path::new(&std::env::var_os("HOME").context("HOME is not set")?)
                    .join("corral")
            }
        };
        Ok(Self { root })
    }

    /// Use `root` as the root directory.
    #[cfg(test)]
    #[must_use]
    pub const fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// The root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Directory holding all runs.
    #[must_use]
    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
    }

    /// Directory holding a symlink to this executable named `corral-podman`.
    #[must_use]
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// File listing patterns that are never snapshotted.
    #[must_use]
    pub fn excludes(&self) -> PathBuf {
        self.root.join("excludes")
    }

    /// Directory of secrets for `project`: one file per secret.
    ///
    /// # Errors
    ///
    /// Returns an error if `project` is not a valid name.
    pub fn secrets(&self, project: &str) -> Result<PathBuf> {
        validate("project", project)?;
        Ok(self.root.join("secrets").join(project))
    }

    /// Bare repository for `project`.
    ///
    /// # Errors
    ///
    /// Returns an error if `project` is not a valid name.
    pub fn repo(&self, project: &str) -> Result<PathBuf> {
        validate("project", project)?;
        Ok(self.root.join("repos").join(format!("{project}.git")))
    }

    /// Paths for a run.
    ///
    /// # Errors
    ///
    /// Returns an error if either name is invalid.
    pub fn run(&self, project: &str, name: &str) -> Result<Run> {
        validate("run", name)?;
        Ok(Run {
            project: project.to_owned(),
            name: name.to_owned(),
            dir: self.runs_dir().join(project).join(name),
            repo: self.repo(project)?,
        })
    }
}

impl Run {
    /// The clone the agent works in. Untrusted.
    #[must_use]
    pub fn workspace(&self) -> PathBuf {
        self.dir.join("workspace")
    }

    /// Export of the base commit, used for config and builds. Trusted.
    #[must_use]
    pub fn pinned(&self) -> PathBuf {
        self.dir.join("pinned")
    }

    /// Files sent to the run, mounted read-only.
    #[must_use]
    pub fn inbox(&self) -> PathBuf {
        self.dir.join("inbox")
    }

    /// Scratch space for the devcontainer CLI.
    #[must_use]
    pub fn tmp(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    /// Metadata file.
    #[must_use]
    pub fn meta(&self) -> PathBuf {
        self.dir.join("run.json")
    }

    /// Branch the agent works on.
    #[must_use]
    pub fn branch(&self) -> String {
        format!("agent/{}", self.name)
    }

    /// Ref that receives snapshots of uncommitted work.
    #[must_use]
    pub fn snapshot_ref(&self) -> String {
        format!("refs/snapshots/{}", self.name)
    }

    /// Label identifying the run's container.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}/{}", self.project, self.name)
    }
}

/// Check that `value` is safe to use as a path component and in a ref name.
pub fn validate(kind: &str, value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !value.ends_with('.')
        && !value.contains("..");
    if !ok {
        bail!(
            "invalid {kind} name {value:?}: use letters, digits, `-`, `_`, \
             and `.`, starting with a letter or digit"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated() {
        for good in ["a", "my-app", "feature_1", "v1.2", "A9"] {
            assert!(validate("x", good).is_ok(), "{good}");
        }
        for bad in [
            "", ".", "..", "-x", ".hidden", "a/b", "a b", "x..y", "x.", "é",
            "a\n",
        ] {
            assert!(validate("x", bad).is_err(), "{bad:?}");
        }
        assert!(validate("x", &"a".repeat(65)).is_err());
    }

    #[test]
    fn paths_are_under_root() {
        let layout = Layout { root: PathBuf::from("/c") };
        let run = layout.run("app", "try1").unwrap();
        assert_eq!(run.repo, Path::new("/c/repos/app.git"));
        assert_eq!(run.workspace(), Path::new("/c/runs/app/try1/workspace"));
        assert_eq!(run.branch(), "agent/try1");
        layout.run("../x", "y").unwrap_err();
        layout.run("app", "../y").unwrap_err();
    }
}
