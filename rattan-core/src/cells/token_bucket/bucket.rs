use bandwidth::Bandwidth;
use bytesize::ByteSize;
use tokio::time::{Duration, Instant};

use crate::cells::token_bucket::LARGE_DURATION;

fn length_to_time(length: ByteSize, rate: Bandwidth) -> Duration {
    if rate.as_bps() == 0 {
        LARGE_DURATION
    } else {
        Duration::from_secs_f64((length.as_u64() as f64 * 8.0) / rate.as_bps() as f64)
    }
}

pub struct TokenBucket {
    fill_up_time: Duration,
    token_rate: Bandwidth,
    time_to_fillup: Duration,
    last_updated: Instant,
}

pub struct Token<'a> {
    bucket: &'a mut TokenBucket,
    pub send_time: Duration,
}

impl<'a> Token<'a> {
    #[inline(always)]
    pub fn consume(self) {
        self.bucket.time_to_fillup += self.send_time;
        debug_assert!(self.bucket.time_to_fillup <= self.bucket.fill_up_time);
    }
}

/// Token bucket driven by a logicol clock.
impl TokenBucket {
    pub fn new(burst_size: ByteSize, token_rate: Bandwidth, timestamp: Instant) -> Self {
        Self {
            fill_up_time: length_to_time(burst_size, token_rate),
            token_rate,
            // A full bucket at init.
            time_to_fillup: Duration::ZERO,
            last_updated: timestamp,
        }
    }

    pub fn next_available(&self, size: ByteSize) -> Instant {
        let send_time = length_to_time(size, self.token_rate);

        if length_to_time(size, self.token_rate) > self.fill_up_time {
            return self.last_updated + LARGE_DURATION;
        }

        if send_time + self.time_to_fillup <= self.fill_up_time {
            return self.last_updated;
        }

        self.last_updated + (send_time + self.time_to_fillup).saturating_sub(self.fill_up_time)
    }

    pub fn update(&mut self, timestamp: Instant) {
        let duration = timestamp.duration_since(self.last_updated);
        if duration <= Duration::ZERO {
            return;
        }
        self.time_to_fillup = self.time_to_fillup.saturating_sub(duration);
        self.last_updated = timestamp;
    }

    pub fn reserve(&mut self, size: ByteSize) -> Option<Token<'_>> {
        let send_time = length_to_time(size, self.token_rate);
        (self.time_to_fillup + send_time <= self.fill_up_time).then_some(Token {
            bucket: self,
            send_time,
        })
    }

    // Allows maintaining the tokens (in Bytes) in bucket during a config change.
    // Not the behavior of current token_bucket
    #[allow(unused)]
    pub fn change_config(
        &mut self,
        burst_size: ByteSize,
        token_rate: Bandwidth,
        timestamp: Instant,
    ) {
        self.update(timestamp);
        let exchange_rate = self.token_rate.div_bandwidth_f64(token_rate);

        let tokens = (self.fill_up_time - self.time_to_fillup).mul_f64(exchange_rate);

        self.fill_up_time = length_to_time(burst_size, token_rate);
        self.time_to_fillup = self.fill_up_time.saturating_sub(tokens);
        self.token_rate = token_rate;
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Example to show how to use 2 token buckets.
    ///
    fn use_2(
        bucket_a: &mut TokenBucket,
        bucket_b: &mut TokenBucket,
        size: ByteSize,
    ) -> Option<ByteSize> {
        if let (Some(token_a), Some(token_b)) = (bucket_a.reserve(size), bucket_b.reserve(size)) {
            // Rust's borrow checker prevents any modification to token_bucket before the token is droped or consumed.
            token_a.consume();
            token_b.consume();
            // Send packet here
            Some(size)
        } else {
            // Nothing changed for both token bucket
            None
        }
    }

    #[test_log::test]
    fn test_two_token_buckets() {
        let start: Instant = Instant::now();
        let mut small_bucket = TokenBucket::new(ByteSize::kb(60), Bandwidth::from_mbps(16), start);
        let mut large_bucket = TokenBucket::new(ByteSize::kb(80), Bandwidth::from_mbps(8), start);

        assert_eq!(
            use_2(&mut small_bucket, &mut large_bucket, ByteSize::kb(50)),
            Some(ByteSize::kb(50))
        );

        // Here, the small bucket remains 10/60 Kb, and fills 2Kb/ms. The large bucket remains 30/80 Kb, and fills 1Kb/ms.
        assert_eq!(
            small_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(start),
            Duration::from_millis(30 / 2)
        );
        assert_eq!(
            large_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(start),
            Duration::from_millis(10)
        );

        // Nothing happens if one of the bucket is not available
        assert_eq!(
            use_2(&mut small_bucket, &mut large_bucket, ByteSize::kb(50)),
            None
        );

        // Here, the small bucket remains 10/60 Kb, and fills 2Kb/ms. The large bucket remains 30/80 Kb, and fills 1Kb/ms.
        assert_eq!(
            small_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(start),
            Duration::from_millis(30 / 2)
        );
        assert_eq!(
            large_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(start),
            Duration::from_millis(10)
        );

        let time = start + Duration::from_millis(13);
        small_bucket.update(time);
        large_bucket.update(time);

        // Here, the small bucket remains 36/60 Kb, and fills 2Kb/ms. The large bucket remains 43/80 Kb, and fills 1Kb/ms.
        assert_eq!(
            small_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(time),
            Duration::from_millis(4 / 2)
        );
        assert_eq!(
            large_bucket
                .next_available(ByteSize::kb(40))
                .duration_since(time),
            Duration::from_millis(0)
        );
    }

    #[test_log::test]
    fn test_token_bucket() {
        let start: Instant = Instant::now();
        let mut token_bucket = TokenBucket::new(ByteSize::kb(10), Bandwidth::from_mbps(1), start);

        // Send 5k at START, available now!
        let send_time = token_bucket.reserve(ByteSize::kb(5)).unwrap();
        assert_eq!(send_time.send_time, Duration::from_millis(40));
        send_time.consume();

        let time = start + Duration::from_millis(8);
        // It takes 8ms to generrate one more 1kB, totaling 6kB.
        assert_eq!(token_bucket.next_available(ByteSize::kb(6)), time);
        assert!(token_bucket.reserve(ByteSize::kb(6)).is_none());

        token_bucket.update(time - Duration::from_millis(4));
        assert_eq!(token_bucket.next_available(ByteSize::kb(6)), time);
        assert!(token_bucket.reserve(ByteSize::kb(6)).is_none());

        token_bucket.update(time);
        assert_eq!(token_bucket.next_available(ByteSize::kb(6)), time);
        assert!(token_bucket
            .reserve(ByteSize::kb(6))
            .is_some_and(|t| t.send_time == Duration::from_millis(48)));
        // Here, at time `time`, the Token bucket is filled with token of 6kB.

        token_bucket.change_config(ByteSize::kb(20), Bandwidth::from_kbps(500), time);
        assert_eq!(token_bucket.next_available(ByteSize::kb(6)), time);
        assert!(token_bucket
            .reserve(ByteSize::kb(6))
            .is_some_and(|t| dbg!(t.send_time) == Duration::from_millis(96)));
        assert_eq!(
            token_bucket.next_available(ByteSize::kb(16)),
            time + Duration::from_millis(160)
        );

        // As the token bucket's capacity is changed to 4 kB, we have an token bucket at `time` fully filled.
        token_bucket.change_config(ByteSize::kb(4), Bandwidth::from_kbps(1000), time);
        assert_eq!(
            token_bucket.next_available(ByteSize::kb(4)),
            time + Duration::from_millis(0)
        );
        let send_time = token_bucket.reserve(ByteSize::kb(3)).unwrap();
        assert_eq!(send_time.send_time, Duration::from_millis(24));
        send_time.consume();

        // Now, the token bucket is at `time` filled with 1kB token.
        assert_eq!(
            token_bucket.next_available(ByteSize::kb(3)),
            time + Duration::from_millis(16)
        );

        // After a long time, the token bucket should be filled with 4kB of token again.
        let time = time + Duration::from_secs(1000);
        token_bucket.update(time);
        let send_time = token_bucket.reserve(ByteSize::kb(3)).unwrap();
        assert_eq!(send_time.send_time, Duration::from_millis(24));
        send_time.consume();
        assert_eq!(
            token_bucket.next_available(ByteSize::kb(3)),
            time + Duration::from_millis(16)
        );
    }
}
