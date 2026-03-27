use std::path::{Path, PathBuf};

use ingot_store_sqlite::Database;
use ingot_store_sqlite::db::sqlite_connect_options;
use ingot_test_support::sqlite::temp_db_path;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

#[allow(dead_code)]
pub async fn migrated_test_db(prefix: &str) -> Database {
    let (db, _) = migrated_test_db_with_path(prefix).await;
    db
}

#[allow(dead_code)]
pub async fn migrated_test_db_with_path(prefix: &str) -> (Database, PathBuf) {
    let path = temp_db_path(prefix);
    let db = Database::connect(&path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    (db, path)
}

#[allow(dead_code)]
pub async fn raw_sqlite_pool(path: &Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlite_connect_options(path, false))
        .await
        .expect("connect raw sqlite pool")
}
