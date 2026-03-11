use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::ProjectId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: String,
    pub default_branch: String,
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
