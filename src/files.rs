//! Files and secrets that live outside git (host side).
//!
//! - `put` unpacks a tar stream into the run's `inbox/`, which the container
//!   sees read-only at `/corral/inbox`. That is for reference files, images, or
//!   whole other repositories.
//! - Secrets are one file per name in the project's secrets directory, which
//!   every run of the project sees read-only at `/run/corral/secrets`. They
//!   never enter git or an image, and `exec` exports them as environment
//!   variables.

use crate::layout::Layout;
use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

/// Largest secret accepted.
const MAX_SECRET_BYTES: u64 = 1024 * 1024;

/// Unpack a tar stream into a run's inbox.
///
/// Only regular files, directories, and symlinks are accepted, and nothing may
/// land outside the inbox.
///
/// # Errors
///
/// Returns an error if the run does not exist or the archive is unsafe.
pub fn put<R: Read>(
    layout: &Layout,
    project: &str,
    name: &str,
    input: R,
) -> Result<()> {
    let run = layout.run(project, name)?;
    if !run.dir.exists() {
        bail!("no such run: {}", run.label());
    }
    let inbox = run.inbox();
    std::fs::create_dir_all(&inbox)?;

    let mut archive = tar::Archive::new(input);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
            bail!("unsupported entry type in archive: {:?}", entry.path());
        }
        if !entry.unpack_in(&inbox)? {
            bail!("unsafe path in archive: {:?}", entry.path());
        }
    }
    Ok(())
}

/// Check that `name` is usable as an environment variable name.
fn validate_secret_name(name: &str) -> Result<()> {
    let ok = name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!(
            "invalid secret name {name:?}: use letters, digits, and `_`, not \
             starting with a digit"
        );
    }
    Ok(())
}

/// Store a secret for `project`, replacing any existing one.
///
/// # Errors
///
/// Returns an error if the name is invalid, the value is too large, or the
/// file cannot be written.
pub fn secret_set<R: Read>(
    layout: &Layout,
    project: &str,
    name: &str,
    input: R,
) -> Result<()> {
    validate_secret_name(name)?;
    let dir = layout.secrets(project)?;
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;

    let mut value = Vec::new();
    input
        .take(MAX_SECRET_BYTES.saturating_add(1))
        .read_to_end(&mut value)?;
    if u64::try_from(value.len())? > MAX_SECRET_BYTES {
        bail!("secret is larger than {MAX_SECRET_BYTES} bytes");
    }

    // Write beside the final file, then rename, so a run never sees half a
    // secret.
    let temp = dir.join(format!(".{name}.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.write_all(&value)?;
    drop(file);
    std::fs::rename(&temp, dir.join(name))?;
    Ok(())
}

/// Print the names of `project`'s secrets (never their values).
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub fn secret_list(
    layout: &Layout,
    project: &str,
    out: &mut dyn Write,
) -> Result<()> {
    let dir = layout.secrets(project)?;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    for name in names {
        writeln!(out, "{name}")?;
    }
    Ok(())
}

/// Remove a secret.
///
/// # Errors
///
/// Returns an error if there is no such secret.
pub fn secret_remove(layout: &Layout, project: &str, name: &str) -> Result<()> {
    validate_secret_name(name)?;
    let path = layout.secrets(project)?.join(name);
    std::fs::remove_file(&path)
        .with_context(|| format!("no such secret: {name}"))
}

/// Shell snippet that exports every secret as an environment variable, then
/// runs the command it was given.
pub const EXPORT_SECRETS_THEN_EXEC: &str = r#"for f in /run/corral/secrets/*; do [ -f "$f" ] && export "$(basename "$f")=$(cat "$f")"; done; exec "$@""#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A layout in a fresh temporary directory, with one run.
    fn setup() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::at(dir.path().to_owned());
        let run = layout.run("p", "r").unwrap();
        std::fs::create_dir_all(&run.dir).unwrap();
        (dir, layout)
    }

    /// Build a tar archive with one file.
    fn archive_with(path: &str, content: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, content).unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn put_unpacks_into_inbox() {
        let (_dir, layout) = setup();
        let data = archive_with("notes/a.txt", b"hello");
        put(&layout, "p", "r", data.as_slice()).unwrap();
        let run = layout.run("p", "r").unwrap();
        assert_eq!(
            std::fs::read(run.inbox().join("notes/a.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn put_refuses_paths_that_escape() {
        let (_dir, layout) = setup();
        let mut data = archive_with("x", b"hi");
        // Rewrite the name in the header to a traversal and fix the checksum.
        let name = b"../escape.txt";
        data[..name.len()].copy_from_slice(name);
        data[name.len()..100].fill(0);
        let mut header = tar::Header::from_byte_slice(&data[..512]).clone();
        header.set_cksum();
        data[..512].copy_from_slice(header.as_bytes());

        put(&layout, "p", "r", data.as_slice()).unwrap_err();
        let run = layout.run("p", "r").unwrap();
        assert!(!run.dir.join("escape.txt").exists());
    }

    #[test]
    fn put_needs_an_existing_run() {
        let (_dir, layout) = setup();
        put(&layout, "p", "missing", archive_with("a", b"x").as_slice())
            .unwrap_err();
    }

    #[test]
    fn secrets_are_private_listed_and_removed() {
        let (_dir, layout) = setup();
        secret_set(&layout, "p", "TOKEN", b"abc".as_slice()).unwrap();
        secret_set(&layout, "p", "TOKEN", b"def".as_slice()).unwrap();
        let path: PathBuf = layout.secrets("p").unwrap().join("TOKEN");
        assert_eq!(std::fs::read(&path).unwrap(), b"def");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let mut out = Vec::new();
        secret_list(&layout, "p", &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "TOKEN\n");

        secret_remove(&layout, "p", "TOKEN").unwrap();
        secret_remove(&layout, "p", "TOKEN").unwrap_err();
    }

    #[test]
    fn secret_names_are_validated() {
        for bad in ["", "1A", "A-B", "A B", "../x", "A=b"] {
            validate_secret_name(bad).unwrap_err();
        }
        for good in ["A", "_x", "GH_TOKEN", "a1"] {
            validate_secret_name(good).unwrap();
        }
    }
}
