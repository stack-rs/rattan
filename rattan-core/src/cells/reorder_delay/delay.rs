use rand_distr::Distribution;
use tokio::time::Duration;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A delay implementation that can be used in the reorder delay cell.
pub trait Delay {
    /// Creates a new delay
    fn new_delay(&self) -> Duration;
}

impl Delay for std::time::Duration {
    fn new_delay(&self) -> Duration {
        *self
    }
}

/// A delay implementation that uses a normal distribution.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub struct NormalLawDelay {
    law: rand_distr::Normal<f64>,
}

/// A delay implementation that uses a log-normal distribution.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug)]
pub struct LogNormalLawDelay {
    law: rand_distr::LogNormal<f64>,
}

impl NormalLawDelay {
    /// Creates a new normal law delay with the specified average and jitter of delay.
    pub fn new(average: Duration, jitter: Duration) -> Result<Self, rand_distr::NormalError> {
        let law = rand_distr::Normal::new(average.as_secs_f64(), jitter.as_secs_f64())?;
        Ok(Self { law })
    }
}

impl LogNormalLawDelay {
    /// Creates a new log-normal law delay with the specified average and jitter of delay.
    ///
    /// The arguments are not the one for the underlying normal distribution, but the real average and jitter
    pub fn new(average: Duration, jitter: Duration) -> Result<Self, rand_distr::NormalError> {
        let m = average.as_secs_f64();
        let s = jitter.as_secs_f64();
        let sigma = f64::sqrt(f64::ln(1.0 + (s * s) / (m * m)));
        let mu = f64::ln(m) - sigma * sigma / 2.;
        let law = rand_distr::LogNormal::new(mu, sigma)?;
        Ok(Self { law })
    }
}

impl Delay for NormalLawDelay {
    fn new_delay(&self) -> Duration {
        Duration::from_secs_f64(self.law.sample(&mut rand::rng()).max(0.0))
    }
}

impl Delay for LogNormalLawDelay {
    fn new_delay(&self) -> Duration {
        Duration::from_secs_f64(self.law.sample(&mut rand::rng()))
    }
}
