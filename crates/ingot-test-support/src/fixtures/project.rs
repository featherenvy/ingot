use std::path::Path;

use chrono::{DateTime, Utc};
use ingot_domain::ids;
use ingot_domain::project::Project;

use super::timestamps::default_timestamp;

pub struct ProjectBuilder {
    id: ids::ProjectId,
    name: String,
    path: String,
    default_branch: String,
    color: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ProjectBuilder {
    pub fn new(path: impl AsRef<Path>) -> Self {
        let now = default_timestamp();
        Self {
            id: ids::ProjectId::new(),
            name: "repo".into(),
            path: path.as_ref().display().to_string(),
            default_branch: "main".into(),
            color: "#000".into(),
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
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
