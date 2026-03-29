use std::path::{Path as FsPath, PathBuf};

use crate::error::ApiError;

pub(crate) fn canonicalize_repo_path(path: &str) -> Result<PathBuf, ApiError> {
    let path = normalize_non_empty("project path", path)?;
    std::fs::canonicalize(path).map_err(|error| ApiError::BadRequest {
        code: "invalid_project_path",
        message: error.to_string(),
    })
}

pub(crate) fn normalize_project_name(
    name: Option<&str>,
    path: &FsPath,
) -> Result<String, ApiError> {
    match name {
        Some(name) => normalize_non_empty("project name", name),
        None => path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ApiError::BadRequest {
                code: "invalid_project_name",
                message: "Project name is required".into(),
            }),
    }
}

pub(crate) fn normalize_project_color(color: Option<&str>) -> Result<String, ApiError> {
    let color = color.unwrap_or("#6366f1").trim().to_lowercase();
    let valid_length = matches!(color.len(), 4 | 7);
    let valid_hex = color.starts_with('#') && color[1..].chars().all(|ch| ch.is_ascii_hexdigit());

    if valid_length && valid_hex {
        Ok(color)
    } else {
        Err(ApiError::BadRequest {
            code: "invalid_project_color",
            message: format!("Invalid project color: {color}"),
        })
    }
}

pub(crate) fn normalize_agent_slug(
    slug: Option<&str>,
    fallback_name: &str,
) -> Result<String, ApiError> {
    let raw = slug.unwrap_or(fallback_name).trim().to_lowercase();
    let mut normalized = String::with_capacity(raw.len());
    let mut previous_dash = false;

    for ch in raw.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            previous_dash = false;
            Some(ch)
        } else if !previous_dash {
            previous_dash = true;
            Some('-')
        } else {
            None
        };

        if let Some(ch) = next {
            normalized.push(ch);
        }
    }

    let normalized = normalized.trim_matches('-').to_string();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_agent_slug",
            message: "Agent slug must contain at least one letter or digit".into(),
        });
    }

    Ok(normalized)
}

pub(crate) fn normalize_non_empty(field: &'static str, value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest {
            code: "invalid_input",
            message: format!("{field} is required"),
        });
    }

    Ok(trimmed.to_string())
}
