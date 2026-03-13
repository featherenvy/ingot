use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_usecases::ProjectLocks;
use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Logging
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,ingot_daemon=debug,ingot_agent_runtime=debug,ingot_agent_adapters=debug",
        )
    });
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .with(env_filter)
        .init();

    tracing::info!("starting ingotd");

    // Database
    let state_root = dirs();
    let db_path = state_root.join("ingot.db");
    let db = ingot_store_sqlite::Database::connect(&db_path).await?;
    db.migrate().await?;
    tracing::info!("database ready at {}", db_path.display());

    let project_locks = ProjectLocks::default();
    let dispatcher = JobDispatcher::new(
        db.clone(),
        project_locks.clone(),
        DispatcherConfig::new(state_root),
    );
    dispatcher.reconcile_startup().await?;
    tokio::spawn(async move {
        dispatcher.run_forever().await;
    });
    tracing::info!("background dispatcher started");

    // HTTP server
    let app = ingot_http_api::build_router_with_project_locks(db.clone(), project_locks);
    let addr = SocketAddr::from(([127, 0, 0, 1], 4190));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("listening on {addr}");
    axum::serve(listener, app).await?;

    Ok(())
}

fn dirs() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = PathBuf::from(home).join(".ingot");
    std::fs::create_dir_all(&path).ok();
    path
}
