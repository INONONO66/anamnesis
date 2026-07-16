//! Configuration for the opt-in extraction worker.

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

/// `warning` is a fixed message that never retains a configured mode value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedExtractMode {
    pub(crate) mode: ExtractMode,
    pub(crate) warning: Option<String>,
}

const UNSUPPORTED_MODE_WARNING: &str = "ANAMNESIS_EXTRACT_MODE is unsupported; extraction is off";

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
            Some(_) => ParsedExtractMode {
                mode: Self::Off,
                warning: Some(UNSUPPORTED_MODE_WARNING.to_owned()),
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
        if program.ends_with('/') || program.ends_with('\\') {
            return Err(ExtractConfigError::InvalidProgramName);
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
    InvalidProgramName,
    NonUnicodeCommand,
}

impl fmt::Display for ExtractConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyCommand => formatter.write_str("ANAMNESIS_EXTRACT_CMD must name a program"),
            Self::InvalidCommand => {
                formatter.write_str("ANAMNESIS_EXTRACT_CMD contains invalid shell-style quoting")
            }
            Self::InvalidProgramName => formatter
                .write_str("ANAMNESIS_EXTRACT_CMD must not end its program with a path separator"),
            Self::NonUnicodeCommand => {
                formatter.write_str("ANAMNESIS_EXTRACT_CMD must be valid Unicode in shadow mode")
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
        let parsed_mode = match std::env::var("ANAMNESIS_EXTRACT_MODE") {
            Ok(mode) => ExtractMode::parse(Some(&mode)),
            Err(std::env::VarError::NotPresent) => ExtractMode::parse(None),
            Err(std::env::VarError::NotUnicode(_)) => ParsedExtractMode {
                mode: ExtractMode::Off,
                warning: Some("ANAMNESIS_EXTRACT_MODE is not valid Unicode".to_owned()),
            },
        };
        let command = match parsed_mode.mode {
            ExtractMode::Off => ExtractCommand::parse(None)?,
            ExtractMode::Shadow => match std::env::var("ANAMNESIS_EXTRACT_CMD") {
                Ok(command) => ExtractCommand::parse(Some(&command))?,
                Err(std::env::VarError::NotPresent) => ExtractCommand::parse(None)?,
                Err(std::env::VarError::NotUnicode(_)) => {
                    return Err(ExtractConfigError::NonUnicodeCommand);
                }
            },
        };

        Ok(Self {
            mode: parsed_mode.mode,
            mode_warning: parsed_mode.warning,
            command,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::{ExtractCommand, ExtractConfig, ExtractConfigError, ExtractMode};
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

        for unsupported in ["auto", "true", "false", "1", "SHADOWED"] {
            let parsed = ExtractMode::parse(Some(unsupported));
            assert_eq!(parsed.mode, ExtractMode::Off, "{unsupported}");
            assert_eq!(
                parsed.warning.as_deref(),
                Some("ANAMNESIS_EXTRACT_MODE is unsupported; extraction is off"),
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
        for command in [
            "",
            "   ",
            "extractor 'unterminated",
            "/usr/local/bin/",
            r"extractor\",
        ] {
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
    #[test]
    fn off_or_invalid_mode_ignores_an_invalid_command() {
        let _lock = ENV_EXTRACT_CONFIG_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = EnvRestore {
            mode: std::env::var_os("ANAMNESIS_EXTRACT_MODE"),
            command: std::env::var_os("ANAMNESIS_EXTRACT_CMD"),
        };

        for mode in ["off", "invalid"] {
            // SAFETY: `ENV_EXTRACT_CONFIG_LOCK` serializes this process-global
            // environment mutation with this module's other extraction config tests.
            unsafe {
                std::env::set_var("ANAMNESIS_EXTRACT_MODE", mode);
                std::env::set_var("ANAMNESIS_EXTRACT_CMD", "extractor 'unterminated");
            }

            let config = ExtractConfig::from_env().expect("off configuration");
            assert_eq!(config.mode, ExtractMode::Off);
            assert_eq!(
                config.command,
                ExtractCommand::parse(None).expect("default command")
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_environment_values_are_handled_without_retaining_their_bytes() {
        use std::os::unix::ffi::OsStringExt;

        let _lock = ENV_EXTRACT_CONFIG_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = EnvRestore {
            mode: std::env::var_os("ANAMNESIS_EXTRACT_MODE"),
            command: std::env::var_os("ANAMNESIS_EXTRACT_CMD"),
        };
        let non_unicode = std::ffi::OsString::from_vec(vec![0xff]);

        // SAFETY: `ENV_EXTRACT_CONFIG_LOCK` serializes this process-global
        // environment mutation with this module's other extraction config tests.
        unsafe {
            std::env::set_var("ANAMNESIS_EXTRACT_MODE", &non_unicode);
            std::env::set_var("ANAMNESIS_EXTRACT_CMD", "extractor 'unterminated");
        }
        let config = ExtractConfig::from_env().expect("non-Unicode mode is fail-open");
        assert_eq!(config.mode, ExtractMode::Off);
        assert_eq!(
            config.mode_warning.as_deref(),
            Some("ANAMNESIS_EXTRACT_MODE is not valid Unicode")
        );

        // SAFETY: `ENV_EXTRACT_CONFIG_LOCK` serializes this process-global
        // environment mutation with this module's other extraction config tests.
        unsafe {
            std::env::set_var("ANAMNESIS_EXTRACT_MODE", "shadow");
            std::env::set_var("ANAMNESIS_EXTRACT_CMD", non_unicode);
        }
        assert_eq!(
            ExtractConfig::from_env(),
            Err(ExtractConfigError::NonUnicodeCommand)
        );
    }
}
