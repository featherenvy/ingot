use std::path::{Path, PathBuf};

use crate::branch_name::BranchName;
use crate::ids;
use crate::project::{ExecutionMode, Project};
use chrono::{DateTime, Utc};

use super::timestamps::default_timestamp;

pub struct ProjectBuilder {
    id: ids::ProjectId,
    name: String,
    path: PathBuf,
    default_branch: BranchName,
    color: String,
    execution_mode: ExecutionMode,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ProjectBuilder {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let now = default_timestamp();
        Self {
            id: ids::ProjectId::new(),
            name: "repo".into(),
            path: path.as_ref().to_path_buf(),
            default_branch: "main".into(),
            color: "#000".into(),
            execution_mode: ExecutionMode::Manual,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn id(mut self, id: ids::ProjectId) -> Self {
        self.id = id;
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = created_at;
        self.updated_at = created_at;
        self
    }

    pub fn build(self) -> Project {
        Project {
            id: self.id,
            name: self.name,
            path: self.path,
            default_branch: self.default_branch,
            color: self.color,
            execution_mode: self.execution_mode,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
