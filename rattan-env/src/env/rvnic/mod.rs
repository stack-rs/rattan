mod af_rvnic;
mod rvnic_env;
mod utils;

use std::sync::Arc;

pub use af_rvnic::{RvnicDriver, RvnicPacket, RvnicReceiver, RvnicSender};
use rvnic::RvnicDevice;
pub use rvnic_env::{get_rvnic_env, RvnicEnv, RvnicEnvConfig, RvnicEnvMode};

pub(crate) mod constants {
    pub const RVNIC_DEIVCES: u32 = 2;
    pub const BATCH_SIZE: usize = 32;
    // Wating at most `SEND_BATCH_MAX_TIME_US` microseconds until trying to send the current batch which does not meet
    // the desired batch size.
    pub const SEND_BATCH_MAX_TIME_US: u64 = 1000;
    // Avoid sleeping for an ultra-short period in a coroutine.
    pub const SEND_BATCH_MIN_SLEEP_US: u64 = 300;
    // Used in epoll call within receiving thread
    pub const RECV_EPOLL_MAX_WAIT_MS: i32 = 1;

    pub const fn default_left_recv_core() -> u32 {
        0
    }
    pub const fn default_right_recv_core() -> u32 {
        2
    }
    pub const fn default_left_napi_core() -> i32 {
        0
    }
    pub const fn default_right_napi_core() -> i32 {
        2
    }

    pub const RATTAN_HEADROOM: u32 = 64;
    pub const fn default_num_queues() -> usize {
        1
    }

    // Re import from rvnic
    pub use rvnic::{RATTAN_DEFAULT_CHUNK_SIZE, RATTAN_RING_SIZE};
}

#[derive(Clone)]
struct DriverMetaData {
    pub self_device: Arc<RvnicDevice>,
    #[allow(unused)]
    pub other_device: Arc<RvnicDevice>,
    pub queue_id: u32,
    pub device_fd: i32,

    pub receive_thread_core: u32,
}

impl DriverMetaData {
    // The sender shall call this after producing packets into the TX Ring
    pub fn kick_self(&self) {
        self.self_device.kick_rx_queue(self.queue_id).ok();
    }
}
