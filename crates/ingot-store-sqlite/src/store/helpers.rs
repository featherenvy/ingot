use std::str::FromStr;

use ingot_domain::ids::{ItemId, ItemRevisionId};
use ingot_domain::ports::RepositoryError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::{Sqlite, Transaction};

#[derive(Debug, thiserror::Error)]
pub(super) enum StoreDecodeError {
    #[error("invalid enum value {value:?}: {message}")]
    Enum { value: String, message: String },
    #[error("invalid json value: {0}")]
    Json(String),
    #[error("invalid id value {value:?}: {message}")]
    Id { value: String, message: String },
}

pub(super) fn parse_enum<T>(value: String) -> Result<T, RepositoryError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(value.clone())).map_err(|err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Enum {
            value,
            message: err.to_string(),
        }))
    })
}

pub(super) fn encode_enum<T>(value: &T) -> Result<String, RepositoryError>
where
    T: Serialize,
{
    match serde_json::to_value(value).map_err(json_err)? {
        serde_json::Value::String(value) => Ok(value),
        other => Err(RepositoryError::Database(Box::new(StoreDecodeError::Json(
            format!("expected string serialization, got {other}"),
        )))),
    }
}

pub(super) fn parse_json<T>(value: String) -> Result<T, RepositoryError>
where
    T: DeserializeOwned,
{
    serde_json::from_str(&value).map_err(|err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Json(format!("{value}: {err}"))))
    })
}

pub(super) fn parse_id<T>(value: String) -> Result<T, RepositoryError>
where
    T: FromStr,
    <T as FromStr>::Err: std::error::Error + Send + Sync + 'static,
{
    value.parse().map_err(|err: <T as FromStr>::Err| {
        RepositoryError::Database(Box::new(StoreDecodeError::Id {
            value,
            message: err.to_string(),
        }))
    })
}

pub(super) fn serialize_optional_json(
    value: Option<&serde_json::Value>,
) -> Result<Option<String>, RepositoryError> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(json_err)
}

pub(super) fn db_err<E>(err: E) -> RepositoryError
where
    E: std::error::Error + Send + Sync + 'static,
{
    RepositoryError::Database(Box::new(err))
}

pub(super) fn db_write_err(err: sqlx::Error) -> RepositoryError {
    match err {
        sqlx::Error::Database(database_error)
            if database_error.is_unique_violation()
                || database_error.is_foreign_key_violation() =>
        {
            RepositoryError::Conflict(database_error.message().to_string())
        }
        other => db_err(other),
    }
}

pub(super) fn json_err(err: serde_json::Error) -> RepositoryError {
    RepositoryError::Database(Box::new(err))
}

pub(super) async fn item_revision_is_stale(
    tx: &mut Transaction<'_, Sqlite>,
    item_id: ItemId,
    expected_item_revision_id: ItemRevisionId,
) -> Result<bool, RepositoryError> {
    let expected_item_revision_id = expected_item_revision_id.to_string();
    let current_revision_id: Option<String> =
        sqlx::query_scalar("SELECT current_revision_id FROM items WHERE id = ?")
            .bind(item_id.to_string())
            .fetch_optional(&mut **tx)
            .await
            .map_err(db_err)?;

    Ok(current_revision_id.as_deref() != Some(expected_item_revision_id.as_str()))
}
