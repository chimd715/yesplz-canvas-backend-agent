//! Descriptor-relative, no-follow checkpoint I/O. No workspace paths are used.
use std::ffi::CString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

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
