use std::collections::VecDeque;

use super::policy::RecycleReason;
use super::types::{PoolEvent, PoolEventType};

pub fn recycle_label(reason: &RecycleReason) -> &'static str {
    reason.as_str()
}

pub fn recent_recycle_reasons(
    events: &VecDeque<PoolEvent>,
    language: &str,
    limit: usize,
) -> Vec<String> {
    events
        .iter()
        .rev()
        .filter(|event| {
            event.language == language
                && matches!(
                    event.event_type,
                    PoolEventType::Recycled | PoolEventType::Quarantined
                )
        })
        .take(limit)
        .map(|event| format!("{}:{}", event.event_type_label(), event.reason))
        .collect()
}

trait PoolEventLabel {
    fn event_type_label(&self) -> &'static str;
}

impl PoolEventLabel for PoolEvent {
    fn event_type_label(&self) -> &'static str {
        match self.event_type {
            PoolEventType::Created => "created",
            PoolEventType::Borrowed => "borrowed",
            PoolEventType::ReturnedIdle => "returned_idle",
            PoolEventType::Recycled => "recycled",
            PoolEventType::Quarantined => "quarantined",
            PoolEventType::Scaled => "scaled",
            PoolEventType::HealthProbePassed => "health_passed",
            PoolEventType::HealthProbeFailed => "health_failed",
            PoolEventType::BorrowTimedOut => "borrow_timed_out",
        }
    }
}
