//! Stable error codes and the single error type used across ccnm.
//!
//! Claude reads the `CCNM_E_*` name out of a failed Bash result, and
//! `ccnm run` keys off the process exit code. Both are a protocol: adding a
//! code is fine, renaming or renumbering one breaks sessions that are already
//! running against an older ccnm on the other machine.

use std::fmt;

/// Every failure ccnm can report. Mirrors design doc section 36.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// A bug or an unexpected OS failure. Not a user-facing category; if a
    /// path keeps landing here it needs its own code.
    Internal,
    /// Doctor found no failure but could not verify everything (a SKIP
    /// row). Nothing is known to be broken, and nothing is proven to work,
    /// so `ccnm run` must still refuse. Distinct from every FAIL code so a
    /// caller can tell "unverified" from "broken".
    NotReady,
    /// Config file missing, unparsable, or fails validation.
    Config,
    /// ccnm binaries on the two machines disagree, or Claude Code is too old.
    Version,
    /// Claude Code on the work machine is not logged in.
    Auth,
    /// Home machine cannot reach the work machine over SSH.
    WorkUnreachable,
    /// Work machine cannot reach the home runner over SSH.
    HomeUnreachable,
    /// SMB share or mount is missing or unusable.
    Mount,
    /// `.ccnm-workspace-id` differs between the mounted view and the home
    /// filesystem, so the two sides are not looking at the same project.
    WrongWorkspace,
    /// A file Claude just wrote does not hash the same on the home machine
    /// yet. The command was not executed.
    Coherence,
    /// The command is not allowed on the runner (source mutation, background
    /// Bash, and so on).
    Policy,
    /// The session's epoch is older than the workspace's current epoch,
    /// typically after `ccnm maintenance`.
    StaleEpoch,
}

impl ErrorCode {
    /// Every code, in the order they are documented.
    pub const ALL: [ErrorCode; 12] = [
        ErrorCode::Internal,
        ErrorCode::NotReady,
        ErrorCode::Config,
        ErrorCode::Version,
        ErrorCode::Auth,
        ErrorCode::WorkUnreachable,
        ErrorCode::HomeUnreachable,
        ErrorCode::Mount,
        ErrorCode::WrongWorkspace,
        ErrorCode::Coherence,
        ErrorCode::Policy,
        ErrorCode::StaleEpoch,
    ];

    /// The `CCNM_E_*` name Claude sees.
    pub fn name(self) -> &'static str {
        match self {
            ErrorCode::Internal => "CCNM_E_INTERNAL",
            ErrorCode::NotReady => "CCNM_E_NOT_READY",
            ErrorCode::Config => "CCNM_E_CONFIG",
            ErrorCode::Version => "CCNM_E_VERSION",
            ErrorCode::Auth => "CCNM_E_AUTH",
            ErrorCode::WorkUnreachable => "CCNM_E_WORK_UNREACHABLE",
            ErrorCode::HomeUnreachable => "CCNM_E_HOME_UNREACHABLE",
            ErrorCode::Mount => "CCNM_E_MOUNT",
            ErrorCode::WrongWorkspace => "CCNM_E_WRONG_WORKSPACE",
            ErrorCode::Coherence => "CCNM_E_COHERENCE",
            ErrorCode::Policy => "CCNM_E_POLICY",
            ErrorCode::StaleEpoch => "CCNM_E_STALE_EPOCH",
        }
    }

    /// Inverse of [`name`](Self::name), for reading a code off another
    /// ccnm's stderr or out of a JSON report.
    pub fn from_name(name: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.into_iter().find(|c| c.name() == name)
    }

    /// Process exit code. Grouped by tens: 1x setup, 2x transport,
    /// 3x workspace state. 0 is success and 2 is reserved for clap usage
    /// errors, so nothing here uses them. 3 sits next to 1 because
    /// "not ready" is a verdict about the whole report, not a category of
    /// failure.
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorCode::Internal => 1,
            ErrorCode::NotReady => 3,
            ErrorCode::Config => 10,
            ErrorCode::Version => 11,
            ErrorCode::Auth => 12,
            ErrorCode::WorkUnreachable => 20,
            ErrorCode::HomeUnreachable => 21,
            ErrorCode::Mount => 22,
            ErrorCode::WrongWorkspace => 30,
            ErrorCode::Coherence => 31,
            ErrorCode::StaleEpoch => 32,
            ErrorCode::Policy => 33,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

type Source = Box<dyn std::error::Error + Send + Sync + 'static>;

/// The one error type. Rendered as
///
/// ```text
/// CCNM_E_COHERENCE:
/// Remote workspace does not match the mounted source view.
/// Command was NOT executed.
/// ```
///
/// so the first line is machine-matchable and the rest is for a human or
/// for Claude.
#[derive(Debug)]
pub struct Error {
    code: ErrorCode,
    message: String,
    source: Option<Source>,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
            source: None,
        }
    }

    pub fn config(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::Config, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Error::new(ErrorCode::Internal, message)
    }

    /// Attach the underlying error. Shown after the message as `caused by:`.
    pub fn with_source(mut self, source: impl Into<Source>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Re-tag an error with a more specific code. Used when a low-level
    /// failure (spawn, I/O) is understood better by the caller: a failed
    /// `ssh` spawn is `WorkUnreachable`, not `Internal`.
    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.code = code;
        self
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:\n{}", self.code.name(), self.message)?;
        if let Some(source) = &self.source {
            write!(f, "\ncaused by: {source}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|s| s as &(dyn std::error::Error + 'static))
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::internal(err.to_string()).with_source(err)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// An error as carried inside a JSON report from the other machine. Keeps
/// the code and message but drops the source chain, which would not
/// serialize and is only meaningful where it happened.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ErrorReport {
    pub code: String,
    pub message: String,
}

impl ErrorReport {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ErrorReport {
            code: code.name().to_string(),
            message: message.into(),
        }
    }

    /// Unknown names map to `Internal`: a newer ccnm may have codes this
    /// one has never heard of.
    pub fn code(&self) -> ErrorCode {
        ErrorCode::from_name(&self.code).unwrap_or(ErrorCode::Internal)
    }
}

impl From<Error> for ErrorReport {
    fn from(err: Error) -> Self {
        ErrorReport::from(&err)
    }
}

impl From<&Error> for ErrorReport {
    fn from(err: &Error) -> Self {
        let mut message = err.message.clone();
        if let Some(source) = &err.source {
            message.push_str("\ncaused by: ");
            message.push_str(&source.to_string());
        }
        ErrorReport {
            code: err.code.name().to_string(),
            message,
        }
    }
}

impl From<ErrorReport> for Error {
    fn from(report: ErrorReport) -> Self {
        Error::new(report.code(), report.message)
    }
}

impl fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// `Result` whose error side survives a trip through JSON.
pub type Reported<T> = std::result::Result<T, ErrorReport>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn from_name_roundtrips_and_rejects_unknown() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_name(code.name()), Some(code));
        }
        assert_eq!(ErrorCode::from_name("CCNM_E_FUTURE"), None);
    }

    #[test]
    fn error_report_keeps_code_and_source_text() {
        let io = std::io::Error::other("disk on fire");
        let err = Error::new(ErrorCode::Mount, "cannot stat").with_source(io);
        let report = ErrorReport::from(&err);
        assert_eq!(report.code(), ErrorCode::Mount);
        assert_eq!(report.message, "cannot stat\ncaused by: disk on fire");
        let json = serde_json::to_string(&report).unwrap();
        let back: ErrorReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
        let err2 = Error::from(back);
        assert_eq!(err2.code(), ErrorCode::Mount);
    }

    #[test]
    fn unknown_report_code_becomes_internal() {
        let report = ErrorReport {
            code: "CCNM_E_FUTURE".into(),
            message: "x".into(),
        };
        assert_eq!(report.code(), ErrorCode::Internal);
    }

    #[test]
    fn names_are_unique_and_prefixed() {
        let names: HashSet<&str> = ErrorCode::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names.len(), ErrorCode::ALL.len());
        for name in names {
            assert!(name.starts_with("CCNM_E_"), "{name}");
        }
    }

    #[test]
    fn exit_codes_are_unique_and_avoid_reserved_values() {
        let codes: HashSet<i32> = ErrorCode::ALL.iter().map(|c| c.exit_code()).collect();
        assert_eq!(codes.len(), ErrorCode::ALL.len());
        assert!(!codes.contains(&0), "0 is success");
        assert!(!codes.contains(&2), "2 is clap usage error");
        for code in codes {
            assert!((1..=255).contains(&code), "{code} must fit in a u8");
        }
    }

    #[test]
    fn display_puts_code_on_its_own_line() {
        let err = Error::new(
            ErrorCode::Coherence,
            "hash mismatch\nCommand was NOT executed.",
        );
        assert_eq!(
            err.to_string(),
            "CCNM_E_COHERENCE:\nhash mismatch\nCommand was NOT executed."
        );
    }

    #[test]
    fn display_appends_source() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err = Error::config("cannot read config").with_source(io);
        assert_eq!(
            err.to_string(),
            "CCNM_E_CONFIG:\ncannot read config\ncaused by: no such file"
        );
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn with_code_retags() {
        let err = Error::internal("spawn failed").with_code(ErrorCode::WorkUnreachable);
        assert_eq!(err.code(), ErrorCode::WorkUnreachable);
        assert_eq!(err.exit_code(), 20);
    }

    #[test]
    fn io_error_converts_to_internal() {
        let err: Error = std::io::Error::other("boom").into();
        assert_eq!(err.code(), ErrorCode::Internal);
        assert_eq!(err.exit_code(), 1);
    }
}
