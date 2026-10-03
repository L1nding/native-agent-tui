#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoreMetrics {
    pub transport_bytes_in: u64,
    pub transport_bytes_out: u64,
    pub control_events: u64,
    pub telemetry_events: u64,
}

impl CoreMetrics {
    pub fn record_incoming(&mut self, method: Option<&str>, has_id: bool) {
        if has_id || !method.is_some_and(is_telemetry_method) {
            self.control_events = self.control_events.saturating_add(1);
        } else {
            self.telemetry_events = self.telemetry_events.saturating_add(1);
        }
    }
}

fn is_telemetry_method(method: &str) -> bool {
    method.ends_with("/delta") || method == "thread/tokenUsage/updated"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_control_and_telemetry_events_without_private_payloads() {
        let mut metrics = CoreMetrics::default();
        metrics.record_incoming(Some("item/agentMessage/delta"), false);
        metrics.record_incoming(Some("turn/completed"), false);
        metrics.record_incoming(Some("item/tool/requestApproval"), true);
        assert_eq!(metrics.telemetry_events, 1);
        assert_eq!(metrics.control_events, 2);
    }
}
