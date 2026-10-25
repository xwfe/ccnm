//! The Unix-only calls ccnm makes from more than one module, and what each
//! one is on Windows, where a Runtime Node is being built a stage at a time
//! (P107–P112, docs/plan/windows-runtime.md).
//!
//! Only calls whose Windows meaning is settled live here. A check that
//! reads ownership or permission bits to decide something does not: on
//! Windows it has no answer yet, and the module that makes it refuses
//! there instead of passing.

use std::io;
use std::path::Path;
use std::process::Command;

#[cfg(unix)]
pub(crate) use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// Windows has no mode bits. What `0o600` / `0o700` ask for on Unix -- this
/// account and no other -- is what a file gets anyway under the account's
/// profile, where ccnm's config and state live (D6): the account,
/// Administrators and SYSTEM. So the mode is accepted and dropped, and a
/// call site reads the same on both.
#[cfg(windows)]
pub(crate) trait OpenOptionsExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(windows)]
impl OpenOptionsExt for std::fs::OpenOptions {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// The directory half of [`OpenOptionsExt`], for the same reason.
#[cfg(windows)]
pub(crate) trait DirBuilderExt {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(windows)]
impl DirBuilderExt for std::fs::DirBuilder {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

/// `chmod`. Nothing on Windows, for the reason [`OpenOptionsExt`] gives.
pub(crate) fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Starts the child in a process group of its own, so what ccnm uses to
/// stop it can reach what it starts too.
///
/// On Windows that is `CREATE_NEW_PROCESS_GROUP`, together with
/// `CREATE_NO_WINDOW`: a console program started from a service (sshd runs
/// in session 0) would otherwise get a console of its own (D3).
pub(crate) fn own_process_group(command: &mut Command) {
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
}

/// Whether `meta` belongs to an account other than `uid`.
///
/// Windows has no uid, and the `id` ccnm reads one from does not exist
/// there, so `uid` is `None` and the answer is no -- as it is on Unix when
/// `id` could not answer. A Windows Runtime only runs as the person's own
/// account (D8), which owns everything in its state directory.
pub(crate) fn owned_by_other(meta: &std::fs::Metadata, uid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        uid.is_some_and(|uid| meta.uid() != uid)
    }
    #[cfg(windows)]
    {
        let _ = (meta, uid);
        false
    }
}

/// Whether `meta` belongs to `uid`. On Windows: not known, so no.
pub(crate) fn owned_by(meta: &std::fs::Metadata, uid: Option<u32>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        uid.is_some_and(|uid| meta.uid() == uid)
    }
    #[cfg(windows)]
    {
        let _ = (meta, uid);
        false
    }
}

/// The error for something that only exists on macOS and Linux: the Agent
/// Node's half of ccnm (P107.1). Windows is a Runtime Node only.
#[cfg(windows)]
pub(crate) fn agent_only(what: &str) -> crate::error::Error {
    crate::error::Error::new(
        crate::error::ErrorCode::NotReady,
        format!(
            "{what} is not supported on Windows: it is the Agent Node's half of ccnm, and an Agent Node is macOS or Linux"
        ),
    )
}

/// Stand-ins for `std::os::unix::net`, which Windows does not have. Only
/// the controller uses Unix sockets, and only on an Agent Node; these let
/// it compile on Windows and fail there before anything is created.
#[cfg(windows)]
pub(crate) mod unix_socket {
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    fn unsupported() -> io::Error {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "Unix sockets are not available on Windows; the controller runs on an Agent Node (macOS or Linux)",
        )
    }

    /// Never constructed: [`UnixListener::bind`] always fails.
    #[derive(Debug)]
    pub(crate) struct UnixListener(std::convert::Infallible);

    impl UnixListener {
        pub(crate) fn bind(_path: impl AsRef<Path>) -> io::Result<UnixListener> {
            Err(unsupported())
        }

        pub(crate) fn accept(&self) -> io::Result<(UnixStream, ())> {
            match self.0 {}
        }
    }

    /// Never constructed: [`UnixStream::connect`] always fails.
    #[derive(Debug)]
    pub(crate) struct UnixStream(std::convert::Infallible);

    impl UnixStream {
        pub(crate) fn connect(_path: impl AsRef<Path>) -> io::Result<UnixStream> {
            Err(unsupported())
        }

        pub(crate) fn set_read_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
            match self.0 {}
        }

        pub(crate) fn set_write_timeout(&self, _timeout: Option<Duration>) -> io::Result<()> {
            match self.0 {}
        }
    }

    impl io::Read for &UnixStream {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            match self.0 {}
        }
    }

    impl io::Write for &UnixStream {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            match self.0 {}
        }

        fn flush(&mut self) -> io::Result<()> {
            match self.0 {}
        }
    }
}
