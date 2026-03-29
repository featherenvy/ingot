use std::fs;
use std::path::PathBuf;

use ingot_store_sqlite::Database;
use uuid::Uuid;

pub fn temp_dir(prefix: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{prefix}-{}", Uuid::now_v7()));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

pub fn temp_state_root(prefix: &str) -> PathBuf {
    temp_dir(prefix)
}

pub async fn migrated_test_db_with_path(prefix: &str) -> (Database, PathBuf) {
    let path = temp_dir(prefix).join("ingot.db");
    let db = Database::connect(&path).await.expect("connect db");
    db.migrate().await.expect("migrate db");
    (db, path)
}

#[cfg(test)]
mod tests {
    use super::{temp_dir, temp_state_root};

    #[test]
    fn temp_dir_creates_unique_directories() {
        let first = temp_dir("ingot-test-support-env");
        let second = temp_dir("ingot-test-support-env");

        assert!(first.exists());
        assert!(second.exists());
        assert_ne!(first, second);
    }

    #[test]
    fn temp_state_root_creates_directory() {
        let root = temp_state_root("ingot-test-support-state");

        assert!(root.exists());
        assert!(root.is_dir());
    }
}
