use crate::PI_RUNTIME_ARGUMENTS_ENV;
use crate::PI_RUNTIME_EXECUTABLE_ENV;
use codex_extension_api::AgentRuntimeError;
use codex_extension_api::AgentRuntimeErrorKind;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PiRuntimeConfig {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

impl PiRuntimeConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            startup_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn from_environment() -> Result<Option<Self>, AgentRuntimeError> {
        let Some(executable) = std::env::var_os(PI_RUNTIME_EXECUTABLE_ENV) else {
            return Ok(None);
        };
        if executable.is_empty() {
            return Err(config_error(format!(
                "{PI_RUNTIME_EXECUTABLE_ENV} must not be empty"
            )));
        }
        let arguments = match std::env::var(PI_RUNTIME_ARGUMENTS_ENV) {
            Ok(value) => serde_json::from_str::<Vec<String>>(&value)
                .map_err(|error| {
                    config_error(format!(
                        "failed to parse {PI_RUNTIME_ARGUMENTS_ENV} as a JSON string array: {error}"
                    ))
                })?
                .into_iter()
                .map(OsString::from)
                .collect(),
            Err(std::env::VarError::NotPresent) => Vec::new(),
            Err(error) => {
                return Err(config_error(format!(
                    "failed to read {PI_RUNTIME_ARGUMENTS_ENV}: {error}"
                )));
            }
        };
        Ok(Some(Self {
            executable: executable.into(),
            arguments,
            ..Self::new(PathBuf::new())
        }))
    }
}

fn config_error(message: impl Into<String>) -> AgentRuntimeError {
    AgentRuntimeError::new(AgentRuntimeErrorKind::InvalidRuntimeId, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_production_timeouts() {
        let config = PiRuntimeConfig::new("bun");
        assert_eq!(config.executable, PathBuf::from("bun"));
        assert_eq!(config.startup_timeout, Duration::from_secs(10));
        assert_eq!(config.request_timeout, Duration::from_secs(30));
    }
}
