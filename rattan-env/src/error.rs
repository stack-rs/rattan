use std::process::{ExitCode, Termination};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("NsError: {0}")]
    NsError(#[from] NsError),
    #[error("VethError: {0}")]
    VethError(#[from] VethError),
    #[error("MacParseError: {0}")]
    MacParseError(#[from] MacParseError),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[error("Config error: {0}")]
    ConfigError(String),
    #[error("Tokio Runtime error: {0}")]
    TokioRuntimeError(#[from] TokioRuntimeError),
    #[error("Rtnetlink error: {0}")]
    RtnetlinkError(#[from] rtnetlink::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum NsError {
    #[error("Can not create netns directory, {0}")]
    CreateNsDirError(std::io::Error),
    #[error("Can not create netns, {0}")]
    CreateNsError(std::io::Error),
    #[error("Can not open netns {0}, {1}")]
    OpenNsError(std::path::PathBuf, std::io::Error),
    #[error("Failed to close netns, {0}")]
    CloseNsError(nix::Error),
    #[error("Failed to mount {0}, {1}")]
    MountError(String, nix::Error),
    #[error("Failed to unmount {0}, {1}")]
    UnmountError(std::path::PathBuf, nix::Error),
    #[error("Failed to unshare, {0}")]
    UnshareError(nix::Error),
    #[error("Failed to join thread, {0}")]
    JoinThreadError(String),
    #[error("Can not setns, {0}")]
    SetNsError(nix::Error),
}

/// An error that may occur when parsing a MAC address string.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, thiserror::Error)]
pub enum MacParseError {
    #[error("Invalid digit")]
    InvalidDigit,
    #[error("Invalid length")]
    InvalidLength,
}

#[derive(Debug, thiserror::Error)]
pub enum VethError {
    #[error("Can not create veth pair, {0}")]
    CreateVethPairError(String),
    #[error("Encounter namespace error, {0}")]
    NsError(#[from] NsError),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[error("Encounter system error, {0}")]
    SystemError(#[from] nix::errno::Errno),
    #[error("Already in namespace {0}")]
    AlreadyInNamespace(String),
    #[error("Set Veth error, {0}")]
    SetError(String),
    #[error("Failed to build veth, {0}")]
    TokioRuntimeError(#[from] TokioRuntimeError),
}

#[derive(Debug, thiserror::Error)]
pub enum TokioRuntimeError {
    #[error("Failed to build runtime, {0}")]
    CreateError(#[from] std::io::Error),
    #[error("Failed to enqueue mpsc, {0}")]
    MpscError(String),
}

impl Termination for Error {
    fn report(self) -> ExitCode {
        match self {
            Error::RtnetlinkError(_) => ExitCode::from(74),
            Error::IoError(_) => ExitCode::from(74),
            Error::TokioRuntimeError(_) => ExitCode::from(74),
            Error::ConfigError(_) => ExitCode::from(78),
            Error::NsError(_) => ExitCode::from(71),
            Error::VethError(_) => ExitCode::from(71),
            Error::MacParseError(_) => ExitCode::from(78),
        }
    }
}
