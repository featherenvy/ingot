mod convergence;
mod finding;
mod item;
mod job;
mod project;
mod revision;
mod timestamps;
mod workspace;

pub use convergence::ConvergenceBuilder;
pub use finding::FindingBuilder;
pub use item::ItemBuilder;
pub use job::JobBuilder;
pub use project::ProjectBuilder;
pub use revision::RevisionBuilder;
pub use timestamps::{DEFAULT_TEST_TIMESTAMP, default_timestamp, parse_timestamp};
pub use workspace::WorkspaceBuilder;

pub fn nil_item() -> ingot_domain::item::Item {
    ItemBuilder::nil().build()
}

pub fn nil_revision() -> ingot_domain::revision::ItemRevision {
    RevisionBuilder::nil()
        .explicit_seed("seed")
        .seed_target_commit_oid(Some("target"))
        .build()
}
