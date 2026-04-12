use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use axum::routing::get;
use ingot_usecases::{UiEvent, UiEventEnvelope};

use super::AppState;

pub(super) fn routes() -> Router<AppState> {
    Router::new().route("/api/ws", get(ws_handler))
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state.ui_events.subscribe()))
}

async fn handle_socket(
    mut socket: WebSocket,
    mut events: tokio::sync::broadcast::Receiver<UiEventEnvelope>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            event = events.recv() => {
                let event = match event {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                };

                let payload = serialize_ws_event(&event);
                if socket
                    .send(Message::Text(payload.to_string().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn serialize_ws_event(event: &UiEventEnvelope) -> serde_json::Value {
    match &event.event {
        UiEvent::EntityChanged(entity) => serde_json::json!({
            "seq": event.seq,
            "event": serde_json::to_value(entity.event_type).expect("activity event type json"),
            "project_id": entity.project_id,
            "entity_type": entity.subject.entity_type(),
            "entity_id": entity.subject.entity_id_string(),
            "payload": entity.payload,
        }),
        UiEvent::JobOutputDelta(chunk) => serde_json::json!({
            "seq": event.seq,
            "event": "job_output_delta",
            "project_id": chunk.project_id,
            "entity_type": "job",
            "entity_id": chunk.job_id,
            "payload": {
                "segment": chunk.segment,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use ingot_agent_protocol::{AgentOutputChannel, AgentOutputKind, AgentOutputSegment};
    use ingot_domain::activity::{ActivityEventType, ActivitySubject};
    use ingot_domain::ids::{ItemId, JobId, ProjectId};
    use ingot_usecases::{EntityChangedEvent, JobOutputDeltaEvent, UiEvent};

    use super::*;

    #[test]
    fn serialize_entity_changed_matches_existing_ws_shape() {
        let project_id = ProjectId::new();
        let item_id = ItemId::new();
        let payload = serialize_ws_event(&UiEventEnvelope {
            seq: 3,
            event: UiEvent::EntityChanged(EntityChangedEvent {
                project_id,
                event_type: ActivityEventType::ItemUpdated,
                subject: ActivitySubject::Item(item_id),
                payload: serde_json::json!({ "x": 1 }),
            }),
        });

        assert_eq!(payload["seq"], 3);
        assert_eq!(payload["event"], "item_updated");
        assert_eq!(payload["project_id"], project_id.to_string());
        assert_eq!(payload["entity_type"], "item");
        assert_eq!(payload["entity_id"], item_id.to_string());
        assert_eq!(payload["payload"], serde_json::json!({ "x": 1 }));
    }

    #[test]
    fn serialize_job_output_delta_includes_segment_payload() {
        let project_id = ProjectId::new();
        let job_id = JobId::new();
        let payload = serialize_ws_event(&UiEventEnvelope {
            seq: 4,
            event: UiEvent::JobOutputDelta(JobOutputDeltaEvent {
                project_id,
                job_id,
                segment: AgentOutputSegment {
                    sequence: 2,
                    channel: AgentOutputChannel::Diagnostic,
                    kind: AgentOutputKind::Text,
                    status: None,
                    title: Some("stderr".into()),
                    text: Some("warn".into()),
                    data: None,
                },
            }),
        });

        assert_eq!(payload["seq"], 4);
        assert_eq!(payload["event"], "job_output_delta");
        assert_eq!(payload["entity_type"], "job");
        assert_eq!(payload["entity_id"], job_id.to_string());
        assert_eq!(payload["payload"]["segment"]["sequence"], 2);
        assert_eq!(payload["payload"]["segment"]["channel"], "diagnostic");
        assert_eq!(payload["payload"]["segment"]["text"], "warn");
    }
}
