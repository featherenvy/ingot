use std::path::PathBuf;

use ingot_usecases::UseCaseError;

use crate::error::ApiError;

pub(crate) async fn read_optional_text(path: PathBuf) -> Result<Option<String>, ApiError> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(ApiError::from(UseCaseError::Internal(error.to_string()))),
    }
}

pub(crate) async fn read_optional_json(
    path: PathBuf,
) -> Result<Option<serde_json::Value>, ApiError> {
    let Some(contents) = read_optional_text(path).await? else {
        return Ok(None);
    };

    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| ApiError::from(UseCaseError::Internal(error.to_string())))
}
