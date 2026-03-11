use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Logging
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env().add_directive("ingot=debug".parse()?))
        .init();

    tracing::info!("starting ingotd");

    // Database
    let db_path = dirs().join("ingot.db");
    let db = ingot_store_sqlite::Database::connect(&db_path).await?;
    db.migrate().await?;
    tracing::info!("database ready at {}", db_path.display());

    // HTTP server
    let app = ingot_http_api::build_router();
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
