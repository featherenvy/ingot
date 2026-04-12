use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ingot_agent_protocol::AgentOutputSegment;
use ingot_domain::activity::{ActivityEventType, ActivitySubject};
use ingot_domain::ids::{JobId, ProjectId};
use tokio::sync::broadcast;

#[derive(Debug, Clone, PartialEq)]
pub struct EntityChangedEvent {
    pub project_id: ProjectId,
    pub event_type: ActivityEventType,
    pub subject: ActivitySubject,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobOutputDeltaEvent {
    pub project_id: ProjectId,
    pub job_id: JobId,
    pub segment: AgentOutputSegment,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiEvent {
    EntityChanged(EntityChangedEvent),
    JobOutputDelta(JobOutputDeltaEvent),
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiEventEnvelope {
    pub seq: u64,
    pub event: UiEvent,
}

#[derive(Clone)]
pub struct UiEventBus {
    inner: Arc<UiEventBusInner>,
}

struct UiEventBusInner {
    sender: broadcast::Sender<UiEventEnvelope>,
    next_seq: AtomicU64,
}

impl UiEventBus {
    pub fn new() -> Self {
        let (sender, _receiver) = broadcast::channel(1024);
        Self {
            inner: Arc::new(UiEventBusInner {
                sender,
                next_seq: AtomicU64::new(0),
            }),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UiEventEnvelope> {
        self.inner.sender.subscribe()
    }

    pub fn publish(&self, event: UiEvent) -> UiEventEnvelope {
        let seq = self
            .inner
            .next_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let envelope = UiEventEnvelope { seq, event };
        let _ = self.inner.sender.send(envelope.clone());
        envelope
    }

    pub fn publish_entity_changed(
        &self,
        project_id: ProjectId,
        event_type: ActivityEventType,
        subject: ActivitySubject,
        payload: serde_json::Value,
    ) -> UiEventEnvelope {
        self.publish(UiEvent::EntityChanged(EntityChangedEvent {
            project_id,
            event_type,
            subject,
            payload,
        }))
    }

    pub fn publish_job_output_delta(
        &self,
        project_id: ProjectId,
        job_id: JobId,
        segment: AgentOutputSegment,
    ) -> UiEventEnvelope {
        self.publish(UiEvent::JobOutputDelta(JobOutputDeltaEvent {
            project_id,
            job_id,
            segment,
        }))
    }
}

impl Default for UiEventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use ingot_domain::activity::{ActivityEventType, ActivitySubject};
    use ingot_domain::ids::{ItemId, JobId, ProjectId};

    use super::*;

    #[tokio::test]
    async fn publish_entity_changed_assigns_increasing_sequence_numbers() {
        let bus = UiEventBus::new();
        let mut rx = bus.subscribe();
        let project_id = ProjectId::new();
        let item_id = ItemId::new();

        let first = bus.publish_entity_changed(
            project_id,
            ActivityEventType::ItemUpdated,
            ActivitySubject::Item(item_id),
            serde_json::json!({ "k": "v" }),
        );
        let second = bus.publish_entity_changed(
            project_id,
            ActivityEventType::ItemUpdated,
            ActivitySubject::Item(item_id),
            serde_json::json!({ "k": "v2" }),
        );

        assert_eq!(first.seq, 1);
        assert_eq!(second.seq, 2);
        assert_eq!(rx.recv().await.expect("first event").seq, 1);
        assert_eq!(rx.recv().await.expect("second event").seq, 2);
    }

    #[tokio::test]
    async fn publish_job_output_delta_broadcasts_typed_payloads() {
        let bus = UiEventBus::new();
        let mut rx = bus.subscribe();
        let project_id = ProjectId::new();
        let job_id = JobId::new();
        let segment = AgentOutputSegment {
            sequence: 1,
            channel: ingot_agent_protocol::AgentOutputChannel::Primary,
            kind: ingot_agent_protocol::AgentOutputKind::Text,
            status: None,
            title: None,
            text: Some("hello".into()),
            data: None,
        };

        bus.publish_job_output_delta(project_id, job_id, segment.clone());

        let event = rx.recv().await.expect("job output delta");
        assert_eq!(event.seq, 1);
        assert_eq!(
            event.event,
            UiEvent::JobOutputDelta(JobOutputDeltaEvent {
                project_id,
                job_id,
                segment,
            })
        );
    }
}
