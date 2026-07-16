//! Configuration for the opt-in extraction worker.
// Task 2 stages these configuration APIs; remove this allowance when Task 7 wires the worker.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "Task 2 staged APIs are consumed by Task 7")
)]

use std::fmt;

/// Extraction execution policy.
///
/// New modes must be explicitly added here: unknown values deliberately disable
/// extraction rather than enabling a worker unexpectedly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtractMode {
    Off,
    Shadow,
}

/// Result of parsing [`ExtractMode`].
///
/// `warning` retains only the unsupported mode value, so callers can report the
/// configuration problem without retaining command or transcript data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedExtractMode {
    pub(crate) mode: ExtractMode,
    pub(crate) warning: Option<String>,
}

impl ExtractMode {
    /// Parse only the exact mode spellings accepted by the extraction contract.
    pub(crate) fn parse(value: Option<&str>) -> ParsedExtractMode {
        match value {
            None | Some("off") => ParsedExtractMode {
                mode: Self::Off,
                warning: None,
            },
            Some("shadow") => ParsedExtractMode {
                mode: Self::Shadow,
                warning: None,
            },
            Some(raw_mode) => ParsedExtractMode {
                mode: Self::Off,
                warning: Some(raw_mode.to_owned()),
            },
        }
    }
}

/// A program and argument vector to execute directly, without a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractCommand {
    pub(crate) program: String,
    pub(crate) args: Vec<String>,
}

impl ExtractCommand {
    /// Parse `ANAMNESIS_EXTRACT_CMD` into an argv vector.
    pub(crate) fn parse(value: Option<&str>) -> Result<Self, ExtractConfigError> {
        let argv = match value {
            None => vec!["claude".to_owned(), "-p".to_owned()],
            Some(command) => {
                shell_words::split(command).map_err(|_| ExtractConfigError::InvalidCommand)?
            }
        };
        let Some((program, args)) = argv.split_first() else {
            return Err(ExtractConfigError::EmptyCommand);
        };
        if program.is_empty() {
            return Err(ExtractConfigError::EmptyCommand);
        }

        Ok(Self {
            program: program.clone(),
            args: args.to_vec(),
        })
    }
}

/// Typed failures in extraction-only configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtractConfigError {
    EmptyCommand,
    InvalidCommand,
}

impl fmt::Display for ExtractConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => formatter.write_str("ANAMNESIS_EXTRACT_CMD must name a program"),
            Self::InvalidCommand => {
                formatter.write_str("ANAMNESIS_EXTRACT_CMD contains invalid shell-style quoting")
            }
        }
    }
}

impl std::error::Error for ExtractConfigError {}

/// Extraction-worker settings resolved independently from the daemon's core
/// configuration so a malformed command cannot affect unrelated operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractConfig {
    pub(crate) mode: ExtractMode,
    pub(crate) mode_warning: Option<String>,
    pub(crate) command: ExtractCommand,
}

impl ExtractConfig {
    /// Resolve extraction settings from `ANAMNESIS_EXTRACT_MODE` and
    /// `ANAMNESIS_EXTRACT_CMD`.
    pub(crate) fn from_env() -> Result<Self, ExtractConfigError> {
        let parsed_mode =
            ExtractMode::parse(std::env::var("ANAMNESIS_EXTRACT_MODE").ok().as_deref());
        let command =
            ExtractCommand::parse(std::env::var("ANAMNESIS_EXTRACT_CMD").ok().as_deref())?;

        Ok(Self {
            mode: parsed_mode.mode,
            mode_warning: parsed_mode.warning,
            command,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::{ExtractCommand, ExtractConfig, ExtractMode};
    /// Extraction configuration tests mutate process-global environment state.
    /// Serialize their set → parse → restore sequence so it cannot self-interleave.
    static ENV_EXTRACT_CONFIG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvRestore {
        mode: Option<std::ffi::OsString>,
        command: Option<std::ffi::OsString>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            // SAFETY: this test holds `ENV_EXTRACT_CONFIG_LOCK` for the entire
            // process-global environment mutation and restoration sequence.
            unsafe {
                match &self.mode {
                    Some(value) => std::env::set_var("ANAMNESIS_EXTRACT_MODE", value),
                    None => std::env::remove_var("ANAMNESIS_EXTRACT_MODE"),
                }
                match &self.command {
                    Some(value) => std::env::set_var("ANAMNESIS_EXTRACT_CMD", value),
                    None => std::env::remove_var("ANAMNESIS_EXTRACT_CMD"),
                }
            }
        }
    }

    #[test]
    fn r2_mode_recognizes_only_off_and_shadow() {
        let default = ExtractMode::parse(None);
        assert_eq!(default.mode, ExtractMode::Off);
        assert!(default.warning.is_none());

        let shadow = ExtractMode::parse(Some("shadow"));
        assert_eq!(shadow.mode, ExtractMode::Shadow);
        assert!(shadow.warning.is_none());

        for unsupported in ["auto", "true", "1", "SHADOWED"] {
            let parsed = ExtractMode::parse(Some(unsupported));
            assert_eq!(parsed.mode, ExtractMode::Off, "{unsupported}");
            assert!(
                parsed
                    .warning
                    .as_deref()
                    .is_some_and(|warning| warning.contains(unsupported)),
                "{unsupported}"
            );
        }
    }

    #[test]
    fn default_command_uses_claude_prompt_argv_without_a_shell() {
        assert_eq!(
            ExtractCommand::parse(None).expect("default command"),
            ExtractCommand {
                program: "claude".into(),
                args: vec!["-p".into()],
            }
        );
    }

    #[test]
    fn custom_command_is_split_into_argv_without_a_shell() {
        assert_eq!(
            ExtractCommand::parse(Some("extractor --model 'test model' --json"))
                .expect("custom argv"),
            ExtractCommand {
                program: "extractor".into(),
                args: vec!["--model".into(), "test model".into(), "--json".into()],
            }
        );
    }

    #[test]
    fn empty_or_invalid_commands_are_rejected() {
        for command in ["", "   ", "extractor 'unterminated"] {
            assert!(ExtractCommand::parse(Some(command)).is_err(), "{command:?}");
        }
    }
    #[test]
    fn extraction_config_reads_mode_and_command_from_environment() {
        let _lock = ENV_EXTRACT_CONFIG_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = EnvRestore {
            mode: std::env::var_os("ANAMNESIS_EXTRACT_MODE"),
            command: std::env::var_os("ANAMNESIS_EXTRACT_CMD"),
        };

        // SAFETY: `ENV_EXTRACT_CONFIG_LOCK` serializes this process-global
        // environment mutation with this module's other extraction config tests.
        unsafe {
            std::env::set_var("ANAMNESIS_EXTRACT_MODE", "shadow");
            std::env::set_var("ANAMNESIS_EXTRACT_CMD", "extractor --json");
        }

        let config = ExtractConfig::from_env().expect("environment config");
        assert_eq!(config.mode, ExtractMode::Shadow);
        assert!(config.mode_warning.is_none());
        assert_eq!(config.command.program, "extractor");
        assert_eq!(config.command.args, ["--json"]);
    }
}
