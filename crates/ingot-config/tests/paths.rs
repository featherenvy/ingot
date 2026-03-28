use std::path::{Path, PathBuf};

use ingot_config::paths::{global_config_path_for_state_root, logs_root, state_root_from_home};

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
}
