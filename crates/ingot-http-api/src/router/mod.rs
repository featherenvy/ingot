mod agents;
mod app;
mod convergence;
mod convergence_port;
mod convergence_route_adapter;
mod core;
mod deps;
mod dispatch;
mod findings;
mod harness;
mod infra_ports;
mod item_projection;
mod items;
mod jobs;
mod projects;
pub(crate) mod support;
#[cfg(test)]
mod test_helpers;
pub(super) mod types;
mod workspaces;

pub(crate) use app::AppState;
pub use app::{
    build_router, build_router_with_project_locks, build_router_with_project_locks_and_state_root,
};
