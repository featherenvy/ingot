use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::branch_name::BranchName;

/// Newtype for Git ref paths (e.g. `refs/heads/main`, `refs/ingot/workspaces/...`),
/// preventing accidental field swaps between different ref-typed parameters.
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct GitRef(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid target ref: {input}")]
pub struct TargetRefParseError {
    input: String,
}

impl TargetRefParseError {
    #[must_use]
    pub fn new(input: impl Into<String>) -> Self {
        Self {
            input: input.into(),
        }
    }

    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }
}

impl GitRef {
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
        Ok(BranchName::parse_target_ref(value)?.to_git_ref())
    }
}

impl fmt::Display for GitRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for GitRef {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl AsRef<str> for GitRef {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for GitRef {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for GitRef {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<&String> for GitRef {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl PartialEq<str> for GitRef {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for GitRef {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::GitRef;

    #[test]
    fn parse_target_ref_prefixes_branch_names() {
        assert_eq!(
            GitRef::parse_target_ref("main").expect("normalize main"),
            "refs/heads/main"
        );
        assert_eq!(
            GitRef::parse_target_ref("refs/heads/release").expect("normalize heads ref"),
            "refs/heads/release"
        );
    }

    #[test]
    fn parse_target_ref_rejects_non_branch_refs() {
        assert_eq!(
            GitRef::parse_target_ref("refs/tags/v1")
                .expect_err("reject tag ref")
                .to_string(),
            "invalid target ref: refs/tags/v1"
        );
        assert_eq!(
            GitRef::parse_target_ref("refs/remotes/origin/main")
                .expect_err("reject remote ref")
                .to_string(),
            "invalid target ref: refs/remotes/origin/main"
        );
    }

    #[test]
    fn parse_target_ref_accepts_valid_branch_names() {
        assert_eq!(
            GitRef::parse_target_ref("feature/ref-hardening").expect("normalize nested branch"),
            "refs/heads/feature/ref-hardening"
        );
        assert_eq!(
            GitRef::parse_target_ref("release-2026.03").expect("normalize dotted branch"),
            "refs/heads/release-2026.03"
        );
        assert_eq!(
            GitRef::parse_target_ref("refs/heads/hotfix_123").expect("normalize full ref"),
            "refs/heads/hotfix_123"
        );
    }

    #[test]
    fn parse_target_ref_only_rejects_empty_branch_names() {
        for invalid_ref in ["", "refs/heads/"] {
            let error = GitRef::parse_target_ref(invalid_ref)
                .err()
                .unwrap_or_else(|| panic!("expected invalid ref: {invalid_ref}"));
            assert_eq!(
                error.to_string(),
                format!("invalid target ref: {invalid_ref}")
            );
        }
    }
}
