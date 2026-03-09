#[cfg(feature = "xdp")]
use camellia_net::error::CamelliaError;

#[derive(Debug, thiserror::Error)]
pub enum MetalError {
    #[error("Encounter system error, {0}")]
    SystemError(#[from] nix::errno::Errno),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[cfg_attr(feature = "xdp", error("Encounter XDP error, {0}"))]
    #[cfg(feature = "xdp")]
    XDPError(#[from] CamelliaError),
    #[error("not interested packet")]
    NotInterestedPacket,
}
