use std::path::{Path, PathBuf};

const STATE_ROOT_DIR_NAME: &str = ".ingot";
const GLOBAL_CONFIG_FILE_NAME: &str = "config.yml";
const LOGS_DIR_NAME: &str = "logs";

pub fn default_state_root() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    state_root_from_home(&home)
}

pub fn state_root_from_home(home: &Path) -> PathBuf {
    home.join(STATE_ROOT_DIR_NAME)
}

pub fn global_config_path() -> PathBuf {
    let state_root = default_state_root();
    global_config_path_for_state_root(&state_root)
}

pub fn global_config_path_for_state_root(state_root: &Path) -> PathBuf {
    state_root.join(GLOBAL_CONFIG_FILE_NAME)
}

pub fn logs_root(state_root: &Path) -> PathBuf {
    state_root.join(LOGS_DIR_NAME)
}
