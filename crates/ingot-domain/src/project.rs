use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::branch_name::BranchName;
use crate::ids::ProjectId;

#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "sqlx", sqlx(rename_all = "snake_case"))]
pub enum ExecutionMode {
    #[default]
    Manual,
    Autopilot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub path: PathBuf,
    pub default_branch: BranchName,
    pub color: String,
    pub execution_mode: ExecutionMode,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
