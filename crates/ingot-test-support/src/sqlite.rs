use std::path::PathBuf;

use ingot_store_sqlite::Database;

use crate::git::unique_temp_path;

pub fn temp_db_path(prefix: &str) -> PathBuf {
    unique_temp_path(prefix).with_extension("db")
}

pub async fn migrated_test_db(prefix: &str) -> Database {
    let path = temp_db_path(prefix);
    let db = Database::connect(&path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    db
}
