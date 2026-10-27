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

/// Children started from here on do not inherit this process's standard
/// input, output and error (P110.1). Call it first thing in `main`.
///
/// Windows hands a child every inheritable handle its parent has, not just
/// the three it was given (rust-lang/rust#38227). This process's output is
/// the ssh channel when it is `mcp-serve`, so a background command would
/// otherwise hold that channel open after the server ends, and the Agent's
/// side would wait for it. Through Windows' own sshd the session's Job ends
/// such a child anyway (P106); started any other way -- CI, a person's own
/// terminal -- nothing would. A child that is given a pipe or `inherit` on
/// purpose still gets it. Unix closes what was not given, so there is
/// nothing to do there.
pub fn keep_std_handles_to_this_process() {
    #[cfg(windows)]
    toexec_win::stop_std_handles_being_inherited();
}

/// End `child` and every process it started that is still in its tree
/// (design D3): `taskkill /T /F` by `taskkill`'s full path, then the child
/// itself directly in case taskkill could not. Returns whether taskkill
/// ran.
///
/// The pid is safe to name while `child` is held: Windows does not reuse a
/// pid while a handle to that process is open, and `Child` holds one. What
/// taskkill cannot find is a process whose parent had already exited -- it
/// follows parent ids -- the same limit as macOS (P106.3).
#[cfg(windows)]
pub(crate) fn kill_tree(child: &mut std::process::Child, taskkill: &std::ffi::OsStr) -> bool {
    let mut command = Command::new(taskkill);
    command
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    own_process_group(&mut command);
    let ran = command.status().is_ok();
    let _ = child.kill();
    ran
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

/// `remove_dir_all`, waiting out a file that is open for a moment.
///
/// On Windows a file another process has open without sharing deletion
/// cannot be removed, and something often has one open just after it was
/// written: Defender scanning it, a command's handle not closed yet. That
/// fails the removal for a few milliseconds and then would not, so a
/// failure is tried again, [`REMOVE_TRIES`] times in all, [`REMOVE_PAUSE`]
/// apart -- about a second, after which whatever holds it is not letting go
/// and the error is the caller's (P109.3). Unix removes open files, so
/// there it is one try.
pub(crate) fn remove_tree(path: &Path) -> io::Result<()> {
    let mut tried = 1;
    loop {
        match std::fs::remove_dir_all(path) {
            Err(e)
                if cfg!(windows) && e.kind() != io::ErrorKind::NotFound && tried < REMOVE_TRIES =>
            {
                tried += 1;
                std::thread::sleep(REMOVE_PAUSE);
            }
            done => return done,
        }
    }
}

const REMOVE_TRIES: u32 = 10;
const REMOVE_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// A program in `%SystemRoot%\System32`, by its full path: `whoami`,
/// `tasklist`, `taskkill`, PowerShell. Never looked up on PATH, where a
/// project's own `bin` could put another program of the same name in front
/// of the one whose answer decides something.
#[cfg(windows)]
pub(crate) fn system_program(name: &str) -> std::path::PathBuf {
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join(name)
}

/// `cmd` as Windows will run it (P110.2). The program is looked up the way
/// a person typing it in `cmd.exe` would expect -- `npm` finds `npm.cmd` --
/// which Rust's `Command` does not do: it only ever adds `.exe`.
///
/// A `.bat` or `.cmd` is left to the standard library, which runs it
/// through `cmd.exe` with the quoting that keeps an argument from becoming
/// a command (CVE-2024-24576), and refuses an argument it cannot quote. A
/// `.ps1` runs under PowerShell, `pwsh` if there is one. Nothing found:
/// `argv` unchanged, for the spawn to fail on and name.
#[cfg(windows)]
pub(crate) fn windows_program(argv: Vec<String>, cwd: &Path) -> Vec<String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let pathext = std::env::var("PATHEXT").ok();
    let Some(found) = find_program(&argv[0], cwd, &path, pathext.as_deref(), &|p: &Path| {
        p.is_file()
    }) else {
        return argv;
    };
    let found = reported_path(&found).to_string_lossy().replace('/', r"\");
    if found.to_ascii_lowercase().ends_with(".ps1") {
        let shell = find_program("pwsh", cwd, &path, pathext.as_deref(), &|p: &Path| {
            p.is_file()
        })
        .unwrap_or_else(|| system_program(r"WindowsPowerShell\v1.0\powershell.exe"));
        let mut out: Vec<String> = [
            &shell.to_string_lossy(),
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &found,
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        out.extend(argv.into_iter().skip(1));
        return out;
    }
    let mut out = argv;
    out[0] = found;
    out
}

/// Where `name` is, the way Windows looks for a program: a path (anything
/// with a separator) relative to `cwd`, a bare name in each PATH directory;
/// either way as given if it has an extension, else with each PATHEXT one
/// in turn. Pure but for `exists`, so both halves are tested everywhere.
#[cfg(any(windows, test))]
fn find_program(
    name: &str,
    cwd: &Path,
    path: &std::ffi::OsStr,
    pathext: Option<&str>,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<std::path::PathBuf> {
    let has_extension = Path::new(name).extension().is_some();
    let names: Vec<String> = if has_extension {
        vec![name.to_string()]
    } else {
        pathext_files(name, pathext)
    };
    let dirs: Vec<std::path::PathBuf> = if name.contains(['/', '\\']) {
        vec![cwd.to_path_buf()]
    } else {
        std::env::split_paths(path).collect()
    };
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| exists(candidate))
}

/// Git for Windows' bash, for `shell` and a skill's commands (design D2,
/// P110.2), in this order:
///
/// 1. `%ProgramFiles%\Git\bin\bash.exe`, where its installer puts it;
/// 2. beside a `git.exe` on PATH: its `cmd\` or `mingw64\bin\` sits under
///    the installation, whose `bin\bash.exe` it is. Scoop's
///    `apps\git\current\cmd` is found this way, its `shims\` is not;
/// 3. `git_bash` in this machine's config.
///
/// Never the `bash.exe` PATH finds first: on a machine with WSL that is
/// `System32\bash.exe`, which starts a Linux distribution, not this.
#[cfg(windows)]
pub(crate) fn git_bash(
    configured: Option<&Path>,
) -> std::result::Result<std::path::PathBuf, String> {
    let installed = std::env::var_os("ProgramFiles")
        .map(|dir| std::path::PathBuf::from(dir).join(r"Git\bin\bash.exe"));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let beside_git = std::env::split_paths(&path)
        .map(|dir| dir.join("git.exe"))
        .filter(|git| git.is_file())
        .find_map(|git| bash_beside_git(&git, &|p: &Path| p.is_file()));
    installed
        .filter(|bash| bash.is_file())
        .or(beside_git)
        .or_else(|| configured.filter(|bash| bash.is_file()).map(Path::to_path_buf))
        .ok_or_else(|| {
            "shell runs its line with Git for Windows' bash, and none was found: not in %ProgramFiles%\\Git, not beside a git.exe on PATH, and git_bash in config.toml names none; install Git for Windows, set git_bash to its bin\\bash.exe, or pass the program and its arguments in cmd instead".to_string()
        })
}

/// `<installation>\bin\bash.exe` for a `git.exe` up to three levels inside
/// that installation.
#[cfg(any(windows, test))]
fn bash_beside_git(git: &Path, exists: &dyn Fn(&Path) -> bool) -> Option<std::path::PathBuf> {
    git.ancestors()
        .skip(1)
        .take(3)
        .map(|dir| dir.join("bin").join("bash.exe"))
        .find(|bash| exists(bash))
}

/// The file names a program called `name` may have here: `name` itself on
/// Unix; on Windows `name` with each extension PATHEXT lists, in its order
/// (`rg.com`, `rg.exe`, ...), since that is what decides what runs there.
pub(crate) fn program_files(name: &str) -> Vec<String> {
    #[cfg(unix)]
    {
        vec![name.to_string()]
    }
    #[cfg(windows)]
    {
        pathext_files(name, std::env::var("PATHEXT").ok().as_deref())
    }
}

/// [`program_files`] for a given PATHEXT. Extensions come back lowercase;
/// an empty or missing PATHEXT means Windows' own default.
#[cfg(any(windows, test))]
fn pathext_files(name: &str, pathext: Option<&str>) -> Vec<String> {
    let listed: Vec<String> = pathext
        .unwrap_or("")
        .split(';')
        .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| !ext.is_empty())
        .collect();
    let extensions = if listed.is_empty() {
        ["com", "exe", "bat", "cmd"].map(String::from).to_vec()
    } else {
        listed
    };
    extensions
        .into_iter()
        .map(|ext| format!("{name}.{ext}"))
        .collect()
}

/// How a path on this machine is written when the Runtime reports or
/// records it: the root `runtime-resolve` gives the Agent, the one
/// `ws add` stores, a machine skill's location.
///
/// On Unix it is the path as it is. On Windows `canonicalize` gives
/// `\\?\C:\Users\x\proj`, and a hand-written config may say `c:\...`; the
/// Agent compares roots as strings (P107.3), so one spelling has to win
/// every time: `C:/Users/x/proj` -- no `\\?\`, `/` throughout, the drive
/// letter in capitals. Only the spelling changes: nothing on disk is
/// looked at, so the case of the rest is what was given.
pub fn reported_path(path: &Path) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        path.to_path_buf()
    }
    #[cfg(windows)]
    {
        windows_spelling(&path.to_string_lossy()).into()
    }
}

/// [`reported_path`]'s Windows half, on a string so every platform tests it.
#[cfg(any(windows, test))]
fn windows_spelling(path: &str) -> String {
    let path = if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    };
    let mut spelled = path.replace('\\', "/");
    if let [drive, b':', ..] = spelled.as_bytes()
        && drive.is_ascii_lowercase()
    {
        let upper = drive.to_ascii_uppercase() as char;
        spelled.replace_range(..1, upper.encode_utf8(&mut [0; 1]));
    }
    spelled
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pathext_order_and_case_decide_the_names() {
        assert_eq!(
            pathext_files("rg", Some(".COM;.EXE;.BAT;.CMD;.VBS;.JS")),
            ["rg.com", "rg.exe", "rg.bat", "rg.cmd", "rg.vbs", "rg.js"]
        );
        // Stray separators and spaces, as a hand-edited PATHEXT has them.
        assert_eq!(
            pathext_files("rg", Some(" .EXE ;; .Cmd;")),
            ["rg.exe", "rg.cmd"]
        );
        for unset in [None, Some(""), Some(";;")] {
            assert_eq!(
                pathext_files("rg", unset),
                ["rg.com", "rg.exe", "rg.bat", "rg.cmd"],
                "{unset:?}"
            );
        }
    }

    /// One spelling whatever Windows or a person wrote: the Agent compares
    /// roots as strings (P107.3).
    #[test]
    fn a_windows_path_is_reported_one_way() {
        for (given, reported) in [
            (r"\\?\C:\Users\x\proj", "C:/Users/x/proj"),
            (r"C:\Users\x\proj", "C:/Users/x/proj"),
            (r"c:\Users\x\proj", "C:/Users/x/proj"),
            ("c:/Users/x/proj", "C:/Users/x/proj"),
            ("C:/Users/x/proj", "C:/Users/x/proj"),
            (r"\\?\D:\", "D:/"),
            (r"\\?\UNC\server\share\proj", "//server/share/proj"),
            (r"\\server\share\proj", "//server/share/proj"),
            (r"C:\Users\张三\项目", "C:/Users/张三/项目"),
        ] {
            assert_eq!(windows_spelling(given), reported, "{given}");
        }
    }

    /// How Windows finds a program, checked against a made-up disk.
    #[test]
    fn a_program_is_found_the_way_windows_finds_one() {
        let bin = Path::new("tools");
        let other = Path::new("more");
        let path = std::env::join_paths([bin, other]).unwrap();
        let disk = [
            bin.join("npm.cmd"),
            other.join("rg.exe"),
            other.join("rg.cmd"),
            Path::new("proj").join("scripts").join("build.bat"),
            other.join("tool.ps1"),
        ];
        let exists = |p: &Path| disk.iter().any(|d| d == p);
        let pathext = Some(".COM;.EXE;.BAT;.CMD");
        let find = |name: &str| find_program(name, Path::new("proj"), &path, pathext, &exists);
        // No extension: PATHEXT decides, in its order, PATH in its order.
        assert_eq!(find("npm"), Some(bin.join("npm.cmd")));
        assert_eq!(find("rg"), Some(other.join("rg.exe")));
        // An extension is taken as given; nothing is added to it.
        assert_eq!(find("tool.ps1"), Some(other.join("tool.ps1")));
        assert_eq!(find("npm.exe"), None);
        // A path is relative to where the command runs, not searched for.
        assert_eq!(
            find("scripts/build"),
            Some(Path::new("proj").join("scripts").join("build.bat"))
        );
        assert_eq!(find("npm/x"), None);
        assert_eq!(find("missing"), None);
    }

    #[test]
    fn git_bash_is_found_from_the_git_beside_it() {
        let scoop = Path::new("scoop").join("apps").join("git").join("current");
        let program_files = Path::new("Git");
        let disk = [
            scoop.join("bin").join("bash.exe"),
            program_files.join("bin").join("bash.exe"),
        ];
        let exists = |p: &Path| disk.iter().any(|d| d == p);
        assert_eq!(
            bash_beside_git(&scoop.join("cmd").join("git.exe"), &exists),
            Some(scoop.join("bin").join("bash.exe"))
        );
        assert_eq!(
            bash_beside_git(
                &program_files.join("mingw64").join("bin").join("git.exe"),
                &exists
            ),
            Some(program_files.join("bin").join("bash.exe"))
        );
        // Scoop's shim stands alone: no installation around it.
        assert_eq!(
            bash_beside_git(&Path::new("scoop").join("shims").join("git.exe"), &exists),
            None
        );
    }

    /// A file held open without sharing deletion, let go of after a while:
    /// on Windows the first tries fail and a later one removes the tree.
    #[test]
    fn a_tree_is_removed_once_its_open_file_is_let_go_of() {
        let dir = std::env::temp_dir().join(format!("ccnm-remove-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("run")).unwrap();
        std::fs::write(dir.join("run/stdout"), "out\n").unwrap();
        let held = held_without_delete(&dir.join("run/stdout"));
        let letting_go = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            drop(held);
        });
        remove_tree(&dir).unwrap();
        letting_go.join().unwrap();
        assert!(!dir.exists());
        // Already gone is an answer, not something to wait out.
        assert_eq!(
            remove_tree(&dir).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    /// Something that never lets go: the error comes back, after about a
    /// second rather than never.
    #[cfg(windows)]
    #[test]
    fn a_file_that_stays_open_is_given_up_on() {
        let dir = std::env::temp_dir().join(format!("ccnm-remove-stuck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stdout"), "out\n").unwrap();
        let held = held_without_delete(&dir.join("stdout"));
        let started = std::time::Instant::now();
        assert!(remove_tree(&dir).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        drop(held);
        remove_tree(&dir).unwrap();
    }

    /// Opened the way an editor or a running program on Windows opens a
    /// file: readable by others, not removable. Elsewhere a plain open.
    fn held_without_delete(path: &Path) -> std::fs::File {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.share_mode(0x1); // FILE_SHARE_READ
        }
        options.open(path).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_path_is_reported_as_it_is() {
        for path in ["/home/x/proj", "/Users/x/a\\b", "relative/x"] {
            assert_eq!(reported_path(Path::new(path)), Path::new(path));
        }
    }
}
