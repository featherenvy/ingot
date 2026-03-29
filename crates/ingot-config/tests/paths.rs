use std::path::{Path, PathBuf};

use ingot_config::paths::{
    database_path_for_state_root, global_config_path_for_state_root, job_logs_dir, logs_root,
    project_config_path, state_root_from_home,
};

#[test]
fn derives_global_paths_from_home_directory() {
    let state_root = state_root_from_home(Path::new("/tmp/ingot-home"));

    assert_eq!(state_root, PathBuf::from("/tmp/ingot-home/.ingot"));
    assert_eq!(
        global_config_path_for_state_root(&state_root),
        PathBuf::from("/tmp/ingot-home/.ingot/config.yml")
    );
    assert_eq!(
        logs_root(&state_root),
        PathBuf::from("/tmp/ingot-home/.ingot/logs")
    );
    assert_eq!(
        database_path_for_state_root(&state_root),
        PathBuf::from("/tmp/ingot-home/.ingot/ingot.db")
    );
    assert_eq!(
        job_logs_dir(&state_root, "job-123"),
        PathBuf::from("/tmp/ingot-home/.ingot/logs/job-123")
    );
    assert_eq!(
        project_config_path(Path::new("/tmp/repo")),
        PathBuf::from("/tmp/repo/.ingot/config.yml")
    );
}
