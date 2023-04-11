#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MacParseError: {0}")]
    MacParseError(#[from] MacParseError),
    #[error("NsError: {0}")]
    NsError(#[from] NsError),
    #[error("VethError: {0}")]
    VethError(#[from] VethError),
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
    /// Parsing of the MAC address contained an invalid digit.
    #[error("Invalid digit")]
    InvalidDigit,
    /// The MAC address did not have the correct length.
    #[error("Invalid length")]
    InvalidLength,
}

#[derive(Debug, thiserror::Error)]
pub enum VethError {
    #[error("Can not create veth pair, {0}")]
    CreateVethPairError(String),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[error("Encounter system error, {0}")]
    SystemError(#[from] nix::errno::Errno),
    #[error("Already in namespace {0}")]
    AlreadyInNamespace(String),
    #[error("Set Veth error, {0}")]
    SetError(String),
}
