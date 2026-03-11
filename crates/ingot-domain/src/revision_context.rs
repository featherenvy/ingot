use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{ItemRevisionId, JobId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContext {
    pub item_revision_id: ItemRevisionId,
    pub schema_version: String,
    pub payload: serde_json::Value,
    pub updated_from_job_id: Option<JobId>,
    pub updated_at: DateTime<Utc>,
}
