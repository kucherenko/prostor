//! A string that must never reach logs: stream keys and the producer secret.

use std::fmt;
use subtle::ConstantTimeEq;

/// Placeholder printed instead of the value.
pub const REDACTED: &str = "<redacted>";

/// A secret string. `Debug` and `Display` print [`REDACTED`]; the value is only
/// reachable through [`Secret::expose_secret`] and [`Secret::scrub`], and compared in
/// constant time through [`Secret::verify`]. Deliberately no `PartialEq`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// The raw value. Keep the result out of logs, errors and `Debug` output.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Length in bytes (safe to log).
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Constant-time comparison with a candidate (the length may still leak).
    pub fn verify(&self, candidate: &str) -> bool {
        self.0.as_bytes().ct_eq(candidate.as_bytes()).into()
    }

    /// Replaces every occurrence of the value in `text` with [`REDACTED`]. Used on
    /// text a remote peer controls before it goes into an event or an error.
    pub fn scrub(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.0, REDACTED)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret({REDACTED})")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_prints_the_value() {
        let secret = Secret::new("hunter2");
        for text in [
            format!("{secret}"),
            format!("{secret:?}"),
            format!("{secret:#?}"),
        ] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(text.contains(REDACTED), "{text}");
        }
        assert_eq!(secret.expose_secret(), "hunter2");
        assert_eq!(secret.len(), 7);
        assert!(!secret.is_empty());
    }

    #[test]
    fn verifies_in_constant_time_and_rejects_prefixes() {
        let secret = Secret::new("hunter2");
        assert!(secret.verify("hunter2"));
        assert!(!secret.verify("hunter"));
        assert!(!secret.verify("hunter22"));
        assert!(!secret.verify(""));
        assert!(Secret::new("").verify(""));
    }

    #[test]
    fn scrubs_every_occurrence() {
        let secret = Secret::new("KEY123");
        assert_eq!(
            secret.scrub("bad name KEY123 (KEY123)"),
            "bad name <redacted> (<redacted>)"
        );
        assert_eq!(secret.scrub("nothing here"), "nothing here");
        assert_eq!(Secret::new("").scrub("a"), "a");
    }
}
