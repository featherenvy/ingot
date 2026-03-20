use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Newtype for Git commit OIDs, preventing accidental field swaps
/// between different OID-typed parameters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitOid(String);

impl CommitOid {
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

impl fmt::Display for CommitOid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for CommitOid {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl AsRef<str> for CommitOid {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for CommitOid {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for CommitOid {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<&String> for CommitOid {
    fn from(s: &String) -> Self {
        Self(s.clone())
    }
}

impl PartialEq<str> for CommitOid {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for CommitOid {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}
