#[derive(Debug, thiserror::Error)]
pub enum MetalError {
    #[error("Encounter system error, {0}")]
    SystemError(#[from] nix::errno::Errno),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[error("not interested packet")]
    NotInterestedPacket,
}