//! Room identifiers as they appear in URLs.
//!
//! A room id is client-supplied, so it is validated before anything (a log line, a file
//! name, a map key) is built from it: `[A-Za-z0-9_-]{1,64}`. Used as an axum `Path`
//! extractor; anything else is a 400.

use serde::{Deserialize, Deserializer, de};
use std::{fmt, str::FromStr};

/// Longest room id accepted.
pub const MAX_ROOM_ID_LEN: usize = 64;

/// A validated room id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomId(String);

/// Why a room id was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomIdError {
    Empty,
    TooLong,
    /// A character outside `[A-Za-z0-9_-]`.
    InvalidCharacter,
}

impl fmt::Display for RoomIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomIdError::Empty => write!(f, "room id is empty"),
            RoomIdError::TooLong => {
                write!(f, "room id is longer than {MAX_ROOM_ID_LEN} characters")
            }
            RoomIdError::InvalidCharacter => {
                write!(f, "room id may only contain letters, digits, '-' and '_'")
            }
        }
    }
}

impl std::error::Error for RoomIdError {}

impl RoomId {
    /// Validates a candidate.
    pub fn parse(candidate: &str) -> Result<Self, RoomIdError> {
        if candidate.is_empty() {
            return Err(RoomIdError::Empty);
        }
        if candidate.len() > MAX_ROOM_ID_LEN {
            return Err(RoomIdError::TooLong);
        }
        if !candidate
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(RoomIdError::InvalidCharacter);
        }
        Ok(RoomId(candidate.to_owned()))
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for RoomId {
    type Err = RoomIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        RoomId::parse(s)
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RoomId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let candidate = String::deserialize(deserializer)?;
        RoomId::parse(&candidate).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_safe_alphabet() {
        for ok in ["a", "room-1", "Room_2", &"x".repeat(MAX_ROOM_ID_LEN)] {
            assert_eq!(RoomId::parse(ok).unwrap().as_str(), ok);
        }
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(RoomId::parse(""), Err(RoomIdError::Empty));
        assert_eq!(
            RoomId::parse(&"x".repeat(MAX_ROOM_ID_LEN + 1)),
            Err(RoomIdError::TooLong)
        );
        for bad in [
            "../../etc/cron.d/x",
            "..",
            ".",
            "a/b",
            "a\\b",
            "a b",
            "nul\0",
            "room\n",
            "ünicode",
            "%2e%2e",
        ] {
            assert_eq!(
                RoomId::parse(bad),
                Err(RoomIdError::InvalidCharacter),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn deserializes_with_validation() {
        let ok: RoomId = serde_json::from_str("\"room-1\"").unwrap();
        assert_eq!(ok.as_str(), "room-1");
        assert!(serde_json::from_str::<RoomId>("\"../x\"").is_err());
        assert!(serde_json::from_str::<RoomId>("\"\"").is_err());
    }
}
