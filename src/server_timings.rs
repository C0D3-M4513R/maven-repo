pub trait AsServerTimingDuration {
    fn as_server_timing_duration(&self) -> f64;
}

impl AsServerTimingDuration for std::time::Duration {
    fn as_server_timing_duration(&self) -> f64 {
        self.as_nanos() as f64 / 1000f64 / 1000f64
    }
}
impl AsServerTimingDuration for std::time::Instant {
    fn as_server_timing_duration(&self) -> f64 {
        (std::time::Instant::now() - *self).as_server_timing_duration()
    }
}
impl AsServerTimingDuration for tokio::time::Instant {
    fn as_server_timing_duration(&self) -> f64 {
        (tokio::time::Instant::now() - *self).as_server_timing_duration()
    }
}