#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreMetrics {
    pub transport_bytes_in: u64,
    pub transport_bytes_out: u64,
    pub control_events: u64,
    pub telemetry_events: u64,
}
