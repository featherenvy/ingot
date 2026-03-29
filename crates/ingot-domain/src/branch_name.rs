use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::git_ref::{GitRef, TargetRefParseError};

/// Newtype for bare branch names (e.g. `main`, `feature/foo`), distinct from
/// full Git refs like `refs/heads/main`.  Use [`BranchName::to_git_ref`] to
/// obtain the corresponding [`GitRef`].
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct BranchName(String);

impl BranchName {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }

    pub fn parse_target_ref(value: &str) -> Result<Self, TargetRefParseError> {
        let branch_name = if let Some(branch_name) = value.strip_prefix("refs/heads/") {
            branch_name
        } else if value.starts_with("refs/") {
            return Err(TargetRefParseError::new(value));
        } else {
            value
        };

        if branch_name.is_empty() {
            return Err(TargetRefParseError::new(value));
        }

        Ok(Self::new(branch_name))
    }

    /// Convert to a full `refs/heads/…` [`GitRef`].
    #[must_use]
    pub fn to_git_ref(&self) -> GitRef {
        GitRef::new(format!("refs/heads/{}", self.0))
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BranchName {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl AsRef<str> for BranchName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for BranchName {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for BranchName {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<&String> for BranchName {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl PartialEq<str> for BranchName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for BranchName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::BranchName;

    #[test]
    fn parse_target_ref_strips_branch_prefix() {
        assert_eq!(
            BranchName::parse_target_ref("main").expect("parse bare branch"),
            "main"
        );
        assert_eq!(
            BranchName::parse_target_ref("refs/heads/release").expect("parse heads ref"),
            "release"
        );
    }
}
