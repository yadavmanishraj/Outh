use std::fmt;

/// Single error type for outh-core. Variants preserve the distinctions the
/// Go code (and the UI) rely on. See CONTRACT.md.
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Http(String),
    Auth(String),
    BadAuthentication,
    NeedsBrowser,
    Protocol(String),
    Config(String),
    AlbumNotFound(String),
    /// A commit returned 2xx with an unparseable body: the media may exist
    /// server-side. NEVER retry these (duplicate risk) — mirrors Go.
    CommitAmbiguous(String),
    Cancelled,
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Http(m) => write!(f, "http error: {m}"),
            Error::Auth(m) => write!(f, "auth error: {m}"),
            Error::BadAuthentication => write!(f, "bad authentication"),
            Error::NeedsBrowser => write!(f, "needs browser sign-in"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Config(m) => write!(f, "config error: {m}"),
            Error::AlbumNotFound(m) => write!(f, "album not found: {m}"),
            Error::CommitAmbiguous(m) => write!(f, "commit result ambiguous: {m}"),
            Error::Cancelled => write!(f, "cancelled"),
            Error::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self { Error::Io(e) }
}
