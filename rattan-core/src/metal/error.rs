#[cfg(feature = "camellia")]
use camellia::error::CamelliaError;

#[derive(Debug, thiserror::Error)]
pub enum MetalError {
    #[error("Encounter system error, {0}")]
    SystemError(#[from] nix::errno::Errno),
    #[error("Encounter IO error, {0}")]
    IoError(#[from] std::io::Error),
    #[cfg_attr(feature = "camellia", error("Encounter XDP error, {0}"))]
    #[cfg(feature = "camellia")]
    XDPError(#[from] CamelliaError),
    #[error("Encounter Rtnetlink error, {0}")]
    RtnetlinkError(#[from] rtnetlink::Error),
    #[error("not interested packet")]
    NotInterestedPacket,
}
