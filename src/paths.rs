//! Remote paths come from the network: they must never leave the shared
//! directory, whatever the client sends.

use std::path::{Component, Path, PathBuf};

/// Name of the directory, inside the shared one, where uploads are written
/// before they are moved into place. Never listed, never addressable.
pub const TMP_DIR: &str = ".fireshare-tmp";

/// Validates a remote path and returns it relative to the shared directory.
///
/// Accepted: one or more plain names separated by `/`. Rejected: empty
/// paths, absolute paths, `.` and `..`, backslashes, NUL, and anything
/// under [`TMP_DIR`].
pub fn clean(remote: &str) -> Result<PathBuf, String> {
    if remote.is_empty() {
        return Err("empty path".into());
    }
    if remote.contains('\\') || remote.contains('\0') {
        return Err("path must use / and contain no NUL".into());
    }
    if remote.starts_with('/') {
        return Err("path must be relative".into());
    }
    let mut out = PathBuf::new();
    for part in remote.split('/') {
        if part.is_empty() {
            return Err("path has an empty segment".into());
        }
        let mut comps = Path::new(part).components();
        match (comps.next(), comps.next()) {
            (Some(Component::Normal(name)), None) => out.push(name),
            _ => return Err(format!("invalid path segment {part:?}")),
        }
    }
    if out.starts_with(TMP_DIR) {
        return Err("reserved path".into());
    }
    Ok(out)
}

/// The remote form of a relative path: `/` separators on every OS.
pub fn to_remote(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_nested_names() {
        assert_eq!(clean("a.txt").unwrap(), PathBuf::from("a.txt"));
        assert_eq!(
            clean("dir/sub/a.tar.gz").unwrap(),
            PathBuf::from("dir/sub/a.tar.gz")
        );
        assert_eq!(clean(".env").unwrap(), PathBuf::from(".env"));
    }

    #[test]
    fn rejects_anything_that_could_escape_or_collide() {
        for bad in [
            "",
            "/etc/passwd",
            "..",
            "../x",
            "a/../../x",
            "a/./b",
            "a//b",
            "a/",
            "a\\b",
            "a\0b",
            ".fireshare-tmp/x",
            ".fireshare-tmp",
        ] {
            assert!(clean(bad).is_err(), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn remote_form_uses_forward_slashes() {
        assert_eq!(to_remote(Path::new("a/b/c.txt")), "a/b/c.txt");
    }
}
