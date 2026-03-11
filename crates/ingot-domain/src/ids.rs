use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! define_id {
    ($name:ident, $prefix:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}_{}", $prefix, self.0.as_simple())
            }
        }
    };
}

define_id!(ProjectId, "prj");
define_id!(AgentId, "agt");
define_id!(ItemId, "itm");
define_id!(ItemRevisionId, "rev");
define_id!(JobId, "job");
define_id!(WorkspaceId, "wrk");
define_id!(ConvergenceId, "conv");
define_id!(GitOperationId, "gop");
define_id!(ActivityId, "act");
