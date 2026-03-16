use crate::agent::{AdapterKind, Agent, AgentCapability, AgentStatus};
use crate::ids;

pub struct AgentBuilder {
    id: ids::AgentId,
    slug: String,
    name: String,
    adapter_kind: AdapterKind,
    provider: String,
    model: String,
    cli_path: String,
    capabilities: Vec<AgentCapability>,
    health_check: Option<String>,
    status: AgentStatus,
}

impl AgentBuilder {
    pub fn new(slug: impl Into<String>, capabilities: Vec<AgentCapability>) -> Self {
        Self {
            id: ids::AgentId::new(),
            slug: slug.into(),
            name: "Codex".into(),
            adapter_kind: AdapterKind::Codex,
            provider: "openai".into(),
            model: "gpt-5-codex".into(),
            cli_path: "codex".into(),
            capabilities,
            health_check: Some("ok".into()),
            status: AgentStatus::Available,
        }
    }

    pub fn id(mut self, id: ids::AgentId) -> Self {
        self.id = id;
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn adapter_kind(mut self, adapter_kind: AdapterKind) -> Self {
        self.adapter_kind = adapter_kind;
        self
    }

    pub fn status(mut self, status: AgentStatus) -> Self {
        self.status = status;
        self
    }

    pub fn build(self) -> Agent {
        Agent {
            id: self.id,
            slug: self.slug,
            name: self.name,
            adapter_kind: self.adapter_kind,
            provider: self.provider,
            model: self.model,
            cli_path: self.cli_path,
            capabilities: self.capabilities,
            health_check: self.health_check,
            status: self.status,
        }
    }
}
