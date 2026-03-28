use chrono::Utc;
use ingot_domain::activity::{Activity, ActivityEventType, ActivitySubject};
use ingot_domain::ids::{ActivityId, ProjectId};

use crate::error::ApiError;
use crate::router::AppState;

use super::errors::repo_to_internal;

pub(crate) async fn append_activity(
    state: &AppState,
    project_id: ProjectId,
    event_type: ActivityEventType,
    subject: ActivitySubject,
    payload: serde_json::Value,
) -> Result<(), ApiError> {
    state
        .db
        .append_activity(&Activity {
            id: ActivityId::new(),
            project_id,
            event_type,
            subject,
            payload,
            created_at: Utc::now(),
        })
        .await
        .map_err(repo_to_internal)
}
