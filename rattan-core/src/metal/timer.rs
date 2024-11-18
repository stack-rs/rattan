use std::os::fd::{AsFd, AsRawFd};

use nix::sys::{
    time::TimeSpec,
    timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags},
};
use tokio::io::unix::AsyncFd;

use crate::metal::error::MetalError;

// High-resolution timer
pub struct Timer {
    timer: AsyncFd<WrapperTimer>,
}

pub struct WrapperTimer(pub TimerFd);

impl AsRawFd for WrapperTimer {
    fn as_raw_fd(&self) -> std::os::unix::io::RawFd {
        self.0.as_fd().as_raw_fd()
    }
}

impl Timer {
    pub fn new() -> Result<Self, MetalError> {
        Ok(Self {
            timer: AsyncFd::new(WrapperTimer(TimerFd::new(
                ClockId::CLOCK_MONOTONIC,
                TimerFlags::TFD_NONBLOCK,
            )?))?,
        })
    }

    pub async fn sleep(&mut self, duration: std::time::Duration) -> Result<(), MetalError> {
        // Set TimerFd to 0 will disable it. We need to handle this case.
        if duration.as_nanos() == 0 {
            return Ok(());
        }
        self.timer.get_mut().0.set(
            Expiration::OneShot(TimeSpec::from_duration(duration)),
            TimerSetTimeFlags::empty(),
        )?;

        let mut buf = [0; 16];
        loop {
            let mut guard = self.timer.readable().await?;
            match guard
                .try_io(|timer| Ok(nix::unistd::read(timer.get_ref().as_raw_fd(), &mut buf)?))
            {
                Ok(timer) => match timer {
                    Ok(_) => return Ok(()),
                    Err(e) => return Err(MetalError::from(e)),
                },
                Err(_would_block) => continue,
            }
        }
    }
}
