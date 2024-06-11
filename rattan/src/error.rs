#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MacParseError: {0}")]
    MacParseError(#[from] MacParseError),
    #[error("NsError: {0}")]
    NsError(#[from] NsError),
    #[error("VethError: {0}")]
    VethError(#[from] VethError),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[error("Metal error: {0}")]
    MetalError(#[from] crate::metal::error::MetalError),
    #[error("Tokio Runtime error: {0}")]
    TokioRuntimeError(#[from] TokioRuntimeError),
    #[error("Rattan radix error: {0}")]
    RattanRadixError(String),
    #[error("Rattan core error: {0}")]
    RattanCoreError(#[from] RattanCoreError),
    #[error("Rattan operation error: {0}")]
    RattanOpError(#[from] RattanOpError),
    #[error("Config error: {0}")]
    ConfigError(String),
    #[error("Channel error: {0}")]
    ChannelError(String),
    #[cfg(feature = "serde")]
    #[error("Serde error: {0}")]
    SerdeError(#[from] serde_json::Error),
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
}

#[derive(Debug, thiserror::Error)]
pub enum RattanCoreError {
    #[error("Failed to spawn rattan task, {0}")]
    SpawnError(String),
    #[error("Failed to add device, {0}")]
    AddDeviceError(String),
    #[error("Failed to send notify, {0}")]
    SendNotifyError(String),
    #[error("Unknown interface ID \"{0}\"")]
    UnknownIdError(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RattanOpError {
    #[error("Failed to send operation {0:?} to rattan controller")]
    SendOpError(crate::control::RattanOp),
    #[error("Failed to receive operation result from rattan controller")]
    RecvOpResError,
    #[error("Operation result mismatch operation type, maybe implement error")]
    MismatchOpResError,
}

#[cfg(feature = "http")]
impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        use axum::response::Json;
        use serde_json::json;

        let status = match self {
            Error::MacParseError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::NsError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::VethError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::IoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::MetalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::TokioRuntimeError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Error::RattanRadixError(_) => StatusCode::BAD_REQUEST,
            Error::RattanCoreError(RattanCoreError::UnknownIdError(_)) => StatusCode::NOT_FOUND,
            Error::RattanCoreError(_) => StatusCode::BAD_REQUEST,
            Error::RattanOpError(_) => StatusCode::BAD_REQUEST,
            Error::ConfigError(_) => StatusCode::BAD_REQUEST,
            Error::ChannelError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            #[cfg(feature = "serde")]
            Error::SerdeError(_) => StatusCode::BAD_REQUEST,
        };

        (status, Json(json!({"error": self.to_string(),}))).into_response()
    }
}
