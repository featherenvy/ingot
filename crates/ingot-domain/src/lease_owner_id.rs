use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Newtype for execution lease owner identity (e.g. `"ingotd:12345"`),
/// preventing accidental reuse with unrelated strings.
#[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(feature = "sqlx", sqlx(transparent))]
pub struct LeaseOwnerId(String);

impl LeaseOwnerId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LeaseOwnerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for LeaseOwnerId {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl From<String> for LeaseOwnerId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for LeaseOwnerId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl PartialEq<str> for LeaseOwnerId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for LeaseOwnerId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}
