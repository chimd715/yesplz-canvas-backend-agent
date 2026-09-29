//! Descriptor-relative, no-follow checkpoint I/O. No workspace paths are used.
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use uuid::Uuid;

struct Directory(File);

fn name(value: &str) -> io::Result<CString> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid checkpoint path component",
        ));
    }
    CString::new(value).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid checkpoint path component"))
}

fn opened(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful open/openat transfers this owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Directory {
    fn root_or_create_last(path: &Path) -> io::Result<Self> {
        match Self::root(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path
                    .parent()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Native session parent is required"))?;
                let child = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Invalid native session directory"))?;
                // Only the final configured component may be missing. Pin its
                // existing parent and never follow a symlink during creation.
                Self::root(parent)?.child(child, true)
            }
            result => result,
        }
    }

    fn root(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Native session directory must be absolute",
            ));
        }
        // SAFETY: constant NUL-terminated pathname, no borrowed descriptor.
        let mut directory = Self(opened(unsafe {
            libc::open(c"/".as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC)
        })?);
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    let part = CString::new(part.as_bytes()).map_err(io::Error::other)?;
                    directory = directory.open_child(&part)?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Invalid native session path",
                    ));
                }
            }
        }
        Ok(directory)
    }

    fn open_child(&self, child: &CString) -> io::Result<Self> {
        // SAFETY: valid descriptor and CString; openat creates an owned fd.
        Ok(Self(opened(unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                child.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?))
    }

    fn child(&self, child: &str, create: bool) -> io::Result<Self> {
        let child = name(child)?;
        if create {
            // SAFETY: valid pinned parent and NUL-terminated single component.
            if unsafe { libc::mkdirat(self.0.as_raw_fd(), child.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
            self.0.sync_all()?;
        }
        let directory = self.open_child(&child)?;
        if create {
            use std::os::unix::fs::MetadataExt;
            let metadata = directory.0.metadata()?;
            if metadata.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Checkpoint directory is not private",
                ));
            }
        }
        Ok(directory)
    }

    fn read(&self, filename: &str, limit: u64) -> io::Result<Vec<u8>> {
        let filename = name(filename)?;
        // O_NONBLOCK avoids blocking on a malicious FIFO before checking type.
        // SAFETY: pinned directory, valid CString; no links followed.
        let file = opened(unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                filename.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        })?;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Native session file exceeds checkpoint policy",
            ));
        }
        let mut bytes = Vec::new();
        file.take(limit + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Native session file exceeds checkpoint limit",
            ));
        }
        Ok(bytes)
    }

    fn write(&self, filename: &str, bytes: &[u8]) -> io::Result<()> {
        let filename = name(filename)?;
        // SAFETY: pinned directory and single-component CString. Never replace.
        let mut file = opened(unsafe {
            libc::openat(
                self.0.as_raw_fd(),
                filename.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600,
            )
        })?;
        file.write_all(bytes)?;
        file.sync_all()
    }
}

pub(super) struct CheckpointFiles {
    sessions: Directory,
    checkpoints: Directory,
}

impl CheckpointFiles {
    pub(super) fn new(session_directory: &Path) -> io::Result<Self> {
        let root = Directory::root(session_directory)?;
        Ok(Self {
            sessions: root.child("sessions", false)?,
            checkpoints: root.child("checkpoints", true)?,
        })
    }

    pub(super) fn read_session(&self, conversation: &str, limit: u64) -> io::Result<Vec<u8>> {
        self.sessions.child(conversation, false)?.read("state.json", limit)
    }

    pub(super) fn publish(&self, conversation: &str, turn: &str, bytes: &[u8], manifest: &[u8]) -> io::Result<()> {
        let parent = self.checkpoints.child(conversation, true)?;
        let pending = format!("pending_{}", Uuid::new_v4());
        let staging = parent.child(&pending, true)?;
        staging.write("state.json", bytes)?;
        staging.write("manifest.json", manifest)?;
        staging.0.sync_all()?;
        let pending = name(&pending)?;
        let turn = name(turn)?;
        // SAFETY: both names refer to this pinned private directory. NOREPLACE
        // rejects duplicate turn IDs; existing immutable checkpoints survive.
        if unsafe {
            libc::renameat2(
                parent.0.as_raw_fd(),
                pending.as_ptr(),
                parent.0.as_raw_fd(),
                turn.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        parent.0.sync_all()
        // Interrupted pending_* directories are intentionally retained and are
        // never a completed checkpoint. The coordinator selects exact turn IDs.
    }
}

/// Fresh private native-manager staging, separate from the live retention list.
/// The pinned directories remain open across SessionManager's path-based I/O.
pub(super) struct ContinuationFiles {
    sessions: Directory,
    staging: Directory,
    _workspace: Directory,
}

pub(super) fn read_prepared_session(session_directory: &Path, conversation: &str, limit: u64) -> io::Result<Vec<u8>> {
    Directory::root(session_directory)?
        .child("sessions", false)?
        .child(conversation, false)?
        .read("state.json", limit)
}

impl ContinuationFiles {
    pub(super) fn new(session_directory: &Path, workspace: &Path) -> io::Result<Self> {
        let workspace = private_workspace(workspace)?;
        let root = Directory::root_or_create_last(session_directory)?;
        let sessions = match root.child("sessions", false) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => root.child("sessions", true)?,
            result => result?,
        };
        let staging = root
            .child("continuations", true)?
            .child(&format!("pending_{}", Uuid::new_v4()), true)?;
        Ok(Self {
            sessions,
            staging,
            _workspace: workspace,
        })
    }

    pub(super) fn manager_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.staging.0.as_raw_fd()))
    }

    pub(super) fn install(&self, id: &str, bytes: &[u8], proof: &[u8]) -> io::Result<()> {
        // Native staging may use upstream default modes; keep the publishable
        // copy private and independently fsynced before the no-replace rename.
        let prepared = self.staging.child("prepared", true)?;
        prepared.write("state.json", bytes)?;
        prepared.write("restore.json", proof)?;
        prepared.0.sync_all()?;
        let source = name("prepared")?;
        let target = name(id)?;
        // SAFETY: pinned parents and single-component names. Existing runtime
        // sessions are never overwritten, including orphaned prior attempts.
        if unsafe {
            libc::renameat2(
                self.staging.0.as_raw_fd(),
                source.as_ptr(),
                self.sessions.0.as_raw_fd(),
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        self.sessions.0.sync_all()?;
        self.staging.0.sync_all()
    }
}

/// Both files are read from the same pinned destination directory.
pub(super) fn read_preparation(
    session_directory: &Path,
    conversation: &str,
    workspace: &Path,
    limit: u64,
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let _workspace = private_workspace(workspace)?;
    let prepared = Directory::root(session_directory)?
        .child("sessions", false)?
        .child(conversation, false)?;
    Ok((
        prepared.read("state.json", limit)?,
        prepared.read("restore.json", 16 * 1024)?,
    ))
}

fn private_workspace(path: &Path) -> io::Result<Directory> {
    use std::os::unix::fs::MetadataExt;
    let workspace = Directory::root(path)?;
    if workspace.0.metadata()?.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Restored workspace is not private",
        ));
    }
    Ok(workspace)
}
