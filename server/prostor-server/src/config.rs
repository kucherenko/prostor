//! Runtime configuration from `PROSTOR_*` environment variables.
//!
//! The configuration fails closed: a secret that is set but unusable (empty or not
//! UTF-8) is an error, never "no auth".

use std::{env, ffi::OsString, fmt, net::SocketAddr, path::PathBuf};

pub use prostor_protocol::secret::Secret;

/// Default bind address: loopback, so a bare `prostor` never listens on the network by
/// accident. The container image sets `PROSTOR_LISTEN=0.0.0.0:8080` explicitly.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8080";

/// Server configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Socket address to bind. `PROSTOR_LISTEN`, default [`DEFAULT_LISTEN`].
    pub listen: SocketAddr,
    /// Directory with the built web app. `PROSTOR_WEB_DIR`, default `web/build`.
    pub web_dir: PathBuf,
    /// Directory for FLV recordings. `PROSTOR_RECORDINGS_DIR`, default `recordings`.
    pub recordings_dir: PathBuf,
    /// Shared producer secret. `PROSTOR_SECRET`; optional until auth lands (M2).
    pub secret: Option<Secret>,
}

/// Errors while reading the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// `PROSTOR_LISTEN` is not a valid socket address.
    InvalidListen(String),
    /// The named variable is set but is not valid UTF-8.
    NotUnicode(&'static str),
    /// `PROSTOR_SECRET` is set but empty.
    EmptySecret,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::InvalidListen(value) => {
                write!(f, "PROSTOR_LISTEN is not a socket address: {value:?}")
            }
            ConfigError::NotUnicode(name) => write!(f, "{name} is not valid UTF-8"),
            ConfigError::EmptySecret => {
                write!(
                    f,
                    "PROSTOR_SECRET is set but empty; unset it or give it a value"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Reads the configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| env::var_os(name))
    }

    /// Reads the configuration through `lookup` (the environment in production, a map in
    /// tests), applying defaults.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<OsString>) -> Result<Self, ConfigError> {
        let text = |name: &'static str| -> Result<Option<String>, ConfigError> {
            match lookup(name) {
                None => Ok(None),
                Some(value) => value
                    .into_string()
                    .map(Some)
                    .map_err(|_| ConfigError::NotUnicode(name)),
            }
        };
        let listen = text("PROSTOR_LISTEN")?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_LISTEN.to_owned());
        let listen = listen
            .parse()
            .map_err(|_| ConfigError::InvalidListen(listen.clone()))?;
        let secret = match lookup("PROSTOR_SECRET") {
            None => None,
            Some(value) => {
                let value = value
                    .into_string()
                    .map_err(|_| ConfigError::NotUnicode("PROSTOR_SECRET"))?;
                if value.is_empty() {
                    return Err(ConfigError::EmptySecret);
                }
                Some(Secret::new(value))
            }
        };
        Ok(Self {
            listen,
            web_dir: path(&lookup, "PROSTOR_WEB_DIR", "web/build"),
            recordings_dir: path(&lookup, "PROSTOR_RECORDINGS_DIR", "recordings"),
            secret,
        })
    }
}

fn path(lookup: &impl Fn(&str) -> Option<OsString>, name: &str, default: &str) -> PathBuf {
    lookup(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_bind_to_loopback_without_a_secret() {
        let config = config(&[]).unwrap();
        assert_eq!(config.listen, DEFAULT_LISTEN.parse().unwrap());
        assert!(config.listen.ip().is_loopback());
        assert_eq!(config.web_dir, PathBuf::from("web/build"));
        assert_eq!(config.recordings_dir, PathBuf::from("recordings"));
        assert!(config.secret.is_none());
    }

    #[test]
    fn reads_every_variable() {
        let config = config(&[
            ("PROSTOR_LISTEN", "0.0.0.0:9000"),
            ("PROSTOR_WEB_DIR", "/app/web"),
            ("PROSTOR_RECORDINGS_DIR", "/recordings"),
            ("PROSTOR_SECRET", "hunter2"),
        ])
        .unwrap();
        assert_eq!(config.listen, "0.0.0.0:9000".parse().unwrap());
        assert_eq!(config.web_dir, PathBuf::from("/app/web"));
        assert_eq!(config.recordings_dir, PathBuf::from("/recordings"));
        assert!(config.secret.as_ref().unwrap().verify("hunter2"));
        assert!(!config.secret.as_ref().unwrap().verify("hunter"));
    }

    #[test]
    fn empty_paths_fall_back_to_defaults_but_an_empty_secret_is_an_error() {
        let cfg = config(&[("PROSTOR_WEB_DIR", ""), ("PROSTOR_LISTEN", "")]).unwrap();
        assert_eq!(cfg.web_dir, PathBuf::from("web/build"));
        assert_eq!(cfg.listen, DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(
            config(&[("PROSTOR_SECRET", "")]).unwrap_err(),
            ConfigError::EmptySecret
        );
    }

    #[test]
    fn invalid_listen_is_reported() {
        assert_eq!(
            config(&[("PROSTOR_LISTEN", "not-an-address")]).unwrap_err(),
            ConfigError::InvalidListen("not-an-address".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_values_fail_closed() {
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'x', 0xFF, 0xFE]);
        let with_bad_secret =
            Config::from_lookup(|name| (name == "PROSTOR_SECRET").then(|| bad.clone()));
        assert_eq!(
            with_bad_secret.unwrap_err(),
            ConfigError::NotUnicode("PROSTOR_SECRET"),
            "a non-UTF-8 secret is an error, not 'no auth'"
        );
        let with_bad_listen =
            Config::from_lookup(|name| (name == "PROSTOR_LISTEN").then(|| bad.clone()));
        assert_eq!(
            with_bad_listen.unwrap_err(),
            ConfigError::NotUnicode("PROSTOR_LISTEN")
        );
    }

    #[test]
    fn secret_is_redacted_in_debug_output() {
        let config = config(&[("PROSTOR_SECRET", "hunter2")]).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("hunter2"), "secret leaked: {debug}");
        assert!(debug.contains("redacted"));
    }
}
