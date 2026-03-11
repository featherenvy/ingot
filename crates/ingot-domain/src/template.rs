use serde::{Deserialize, Serialize};

use crate::job::PhaseKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTemplate {
    pub slug: String,
    pub phase_kind: PhaseKind,
    pub prompt: String,
    pub enabled: bool,
}
