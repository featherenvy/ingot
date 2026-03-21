use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Newtype for Git ref paths (e.g. `refs/heads/main`, `refs/ingot/workspaces/...`),
/// preventing accidental field swaps between different ref-typed parameters.
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct GitRef(String);

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
