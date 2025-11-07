use std::fmt::Debug;

use tokio::time::{Duration, Instant};

pub fn relative_time(time: Instant) -> Duration {
    let start = crate::cells::TRACE_START_INSTANT.get_or_init(Instant::now);
    time.duration_since(*start)
}

/// Store the current active config and next config.
#[derive(Debug, Default)]
pub struct IncomingConfigs<C: Clone> {
    fallback: Option<C>,
    current: Option<(Instant, Instant, C)>,
    next: Option<C>,
}

impl<C: Clone + Debug> IncomingConfigs<C> {
    pub fn reset(&mut self) {
        dbg!("reset");
        self.current = None;
        self.next = None;
        self.fallback = None;
    }

    /// The value should be current_value during the period [valid_since, valid_duration)
    /// And it is expected to be next_value(if some) after valid_duration.
    pub fn update(
        &mut self,
        current_value: C,
        next_value: Option<C>,
        valid_since: Instant,
        valid_duration: Duration,
    ) {
        self.fallback = self.current.take().map(|(_, _, value)| value);
        self.current = (valid_since, valid_since + valid_duration, current_value).into();
        self.next = next_value;
    }

    /// Get the config value for given timestamp.
    /// It is expected that the timestamp passed into this function is non-decending.
    ///
    /// According to the given timestamp,
    ///
    /// 0)  If `current` value is not set.
    ///     a) Try to use the `fallback`
    ///
    /// 1)  If `current` value has not yet taken effect,
    ///     a) Try to use the `fallback`
    ///     b) Use the `current` value
    ///
    /// 2)  If `current` value is in its validity period,
    ///     Use the current value
    ///
    /// 3)  If `current` value has expired,
    ///     a) Try to use the expected `next_value`, if some
    ///     b) Use the `current` value
    ///
    pub fn get_current(&self, timestamp: Instant) -> Option<C> {
        if let Some((valid_since, expiry, value)) = self.current.as_ref() {
            if (*valid_since..=*expiry).contains(&timestamp) {
                return value.clone().into();
            }
            if *expiry <= timestamp {
                return self.next.clone().or(value.clone().into());
            }
            return self.fallback.clone().or(value.clone().into());
        }
        self.fallback.clone()
    }
}
