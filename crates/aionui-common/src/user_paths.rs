//! Per-user on-disk directory naming (multi-account filesystem isolation).
//!
//! The directory name is the Core user id with its `user_` prefix stripped:
//! readable, operable, and shorter than the full id for Windows path budget.
//! `system_default_user` has no prefix and is used verbatim.

use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserDirNameError {
    Empty,
    Unsafe(String),
}

impl fmt::Display for UserDirNameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserDirNameError::Empty => write!(f, "empty user id"),
            UserDirNameError::Unsafe(id) => write!(f, "user id is not filename-safe: {id}"),
        }
    }
}

impl std::error::Error for UserDirNameError {}

/// Directory name for a user's per-user on-disk root, e.g.
/// `user_019f8de8-…` -> `019f8de8-…`, `system_default_user` -> unchanged.
pub fn user_dir_name(user_id: &str) -> Result<String, UserDirNameError> {
    let trimmed = user_id.trim();
    if trimmed.is_empty() {
        return Err(UserDirNameError::Empty);
    }
    let name = trimmed.strip_prefix("user_").unwrap_or(trimmed);
    if name.is_empty() || !is_filename_safe(name) {
        return Err(UserDirNameError::Unsafe(user_id.to_owned()));
    }
    Ok(name.to_owned())
}

/// A caller-owned namespace beneath the managed upload root. Full IDs avoid
/// collisions between prefixed and non-prefixed identities.
pub fn user_upload_root(root: &Path, user_id: &str) -> Result<PathBuf, UserDirNameError> {
    user_dir_name(user_id)?;
    Ok(root.join("users").join(user_id.trim()))
}

/// Authorize only managed attachments; unrelated host paths retain their
/// existing sandbox policy. Resolve aliases and missing write destinations so
/// switching a reference kind or using a legacy filesystem API cannot bypass
/// attachment ownership.
pub fn managed_upload_access_allowed(root: &Path, user_id: &str, candidate: &Path) -> bool {
    let Some(absolute_root) = absolute_normalized(root) else {
        return false;
    };
    let Some(absolute_candidate) = absolute_normalized(candidate) else {
        return false;
    };
    let Some(canonical_root) = resolve_existing_ancestor(root) else {
        return false;
    };
    let Some(canonical_candidate) = resolve_existing_ancestor(candidate) else {
        return false;
    };
    // Bulk operations on a parent directory must not read/copy the protected
    // subtree by naming its ancestor instead of an attachment path.
    if canonical_candidate.is_dir() && canonical_root.starts_with(&canonical_candidate) {
        return false;
    }
    let managed = absolute_candidate.starts_with(&absolute_root) || canonical_candidate.starts_with(&canonical_root);
    if !managed {
        return true;
    }
    let Ok(owned_root) = user_upload_root(&canonical_root, user_id) else {
        return false;
    };
    canonical_candidate.starts_with(&owned_root)
        || (user_id == "system_default_user"
            && canonical_candidate != canonical_root
            && canonical_candidate.starts_with(&canonical_root)
            && !canonical_candidate.starts_with(canonical_root.join("users")))
}

fn absolute_normalized(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Some(normalized)
}

/// Canonicalize the nearest existing ancestor, then append missing components.
/// This also checks destinations before an API creates a file or directory.
fn resolve_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut canonical) = std::fs::canonicalize(&ancestor) {
            for component in missing.iter().rev() {
                canonical.push(component);
            }
            return absolute_normalized(&canonical);
        }
        missing.push(ancestor.file_name()?.to_os_string());
        ancestor = ancestor.parent()?.to_path_buf();
    }
}

fn is_filename_safe(name: &str) -> bool {
    if name == "." || name == ".." {
        return false;
    }
    name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("upload-guard-{}", uuid::Uuid::now_v7()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn strips_user_prefix() {
        assert_eq!(
            user_dir_name("user_019f8de8-3537-7c73-8d92-3bfde17eb1ee").unwrap(),
            "019f8de8-3537-7c73-8d92-3bfde17eb1ee"
        );
    }

    #[test]
    fn default_user_is_verbatim() {
        assert_eq!(user_dir_name("system_default_user").unwrap(), "system_default_user");
    }

    #[test]
    fn rejects_empty_and_unsafe() {
        assert_eq!(user_dir_name(""), Err(UserDirNameError::Empty));
        assert!(matches!(user_dir_name("user_"), Err(UserDirNameError::Unsafe(_))));
        assert!(matches!(user_dir_name("user_a/b"), Err(UserDirNameError::Unsafe(_))));
        assert!(matches!(user_dir_name("user_.."), Err(UserDirNameError::Unsafe(_))));
        assert!(matches!(user_dir_name("../etc"), Err(UserDirNameError::Unsafe(_))));
    }
    #[test]
    fn upload_ownership_covers_existing_and_missing_destinations() {
        let temp = TestDirectory::new();
        let root = temp.path().join("uploads");
        let alice = user_upload_root(&root, "user_alice").unwrap();
        let bob = user_upload_root(&root, "user_bob").unwrap();
        std::fs::create_dir_all(&alice).unwrap();
        std::fs::create_dir_all(&bob).unwrap();
        let file = alice.join("note.txt");
        std::fs::write(&file, b"private bytes").unwrap();
        assert!(managed_upload_access_allowed(&root, "user_alice", &file));
        assert!(!managed_upload_access_allowed(&root, "user_bob", &file));
        assert!(!managed_upload_access_allowed(&root, "system_default_user", &file));
        assert!(managed_upload_access_allowed(
            &root,
            "user_alice",
            &alice.join("new/file.txt")
        ));
        assert!(!managed_upload_access_allowed(
            &root,
            "user_bob",
            &alice.join("new/file.txt")
        ));
        assert!(!managed_upload_access_allowed(&root, "user_alice", &root));
        assert!(!managed_upload_access_allowed(&root, "user_alice", temp.path()));
        assert!(!managed_upload_access_allowed(&root, "user_alice", &root.join("users")));
        assert!(!managed_upload_access_allowed(
            &root,
            "user_alice",
            &alice.join("../user_bob/new.txt")
        ));
        assert!(!managed_upload_access_allowed(
            &root,
            "user_alice",
            &alice.join("missing/../../user_bob/new.txt")
        ));
    }

    #[test]
    fn legacy_uploads_stay_local_and_unmanaged_paths_keep_their_policy() {
        let temp = TestDirectory::new();
        let root = temp.path().join("uploads");
        std::fs::create_dir_all(root.join("general")).unwrap();
        let legacy = root.join("general/legacy.txt");
        std::fs::write(&legacy, b"legacy").unwrap();
        assert!(managed_upload_access_allowed(&root, "system_default_user", &legacy));
        assert!(!managed_upload_access_allowed(&root, "user_alice", &legacy));
        assert!(managed_upload_access_allowed(
            &root,
            "user_alice",
            &temp.path().join("workspace/new.txt")
        ));
        assert_ne!(
            user_upload_root(&root, "user_alice").unwrap(),
            user_upload_root(&root, "alice").unwrap()
        );
    }

    #[test]
    #[cfg(unix)]
    fn upload_ownership_checks_symlinks_from_inside_and_outside_the_root() {
        let temp = TestDirectory::new();
        let root = temp.path().join("uploads");
        let alice = user_upload_root(&root, "user_alice").unwrap();
        let bob = user_upload_root(&root, "user_bob").unwrap();
        std::fs::create_dir_all(&alice).unwrap();
        std::fs::create_dir_all(&bob).unwrap();
        let private = bob.join("private.txt");
        std::fs::write(&private, b"private").unwrap();
        let alias = temp.path().join("alias.txt");
        std::os::unix::fs::symlink(&private, &alias).unwrap();
        assert!(!managed_upload_access_allowed(&root, "user_alice", &alias));
        std::os::unix::fs::symlink(&bob, alice.join("redirect")).unwrap();
        assert!(!managed_upload_access_allowed(
            &root,
            "user_alice",
            &alice.join("redirect/new.txt")
        ));
        let outside = temp.path().join("outside.txt");
        std::fs::write(&outside, b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, alice.join("escaped.txt")).unwrap();
        assert!(!managed_upload_access_allowed(
            &root,
            "user_alice",
            &alice.join("escaped.txt")
        ));
    }
}
