use std::net::SocketAddr;
use std::path::Path;

use anyhow::Result;
use ingot_agent_runtime::{DispatcherConfig, JobDispatcher};
use ingot_config::paths::{database_path_for_state_root, default_state_root, logs_root};
use ingot_usecases::{DispatchNotify, ProjectLocks, UiEventBus};
use tokio::net::TcpListener;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    let state_root = default_state_root();
    std::fs::create_dir_all(&state_root).ok();
    let _file_log_guard = init_tracing(&logs_root(&state_root))?;

    tracing::info!("starting ingotd");

    // Database
    let db_path = database_path_for_state_root(&state_root);
    let db = ingot_store_sqlite::Database::connect(&db_path).await?;
    db.migrate().await?;
    tracing::info!("database ready at {}", db_path.display());

    let project_locks = ProjectLocks::default();
    let dispatch_notify = DispatchNotify::default();
    let ui_events = UiEventBus::default();
    let dispatcher = JobDispatcher::new_with_events(
        db.clone(),
        project_locks.clone(),
        DispatcherConfig::new(state_root.clone()),
        dispatch_notify.clone(),
        ui_events.clone(),
    );
    dispatcher.reconcile_startup().await?;
    tokio::spawn(async move {
        dispatcher.run_forever().await;
    });
    tracing::info!("background dispatcher started");

    // HTTP server
    let app = ingot_http_api::build_router_with_project_locks_and_state_root_and_events(
        db.clone(),
        project_locks,
        state_root.clone(),
        dispatch_notify,
        ui_events,
    );
    let addr = SocketAddr::from(([127, 0, 0, 1], 4190));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("listening on {addr}");
    axum::serve(listener, app).await?;

    Ok(())
}

fn init_tracing(log_dir: &Path) -> Result<WorkerGuard> {
    std::fs::create_dir_all(log_dir)?;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(
            "info,ingot_daemon=debug,ingot_agent_runtime=debug,ingot_agent_adapters=debug",
        )
    });
    let file_appender = tracing_appender::rolling::never(log_dir, "daemon.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_writer(std::io::stdout)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .init();

    Ok(file_guard)
}
