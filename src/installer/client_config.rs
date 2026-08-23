use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt,
    path::{Path, PathBuf},
};

use super::InstallError;

const SECRET_ENV_NAMES: &[&str] = &[
    "MICU_API_KEY",
    "MICU_GROK_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
];

#[derive(Clone, Eq, PartialEq)]
pub struct ClientLaunchSpec {
    command: PathBuf,
    args: Vec<OsString>,
    env: BTreeMap<String, String>,
}

impl ClientLaunchSpec {
    pub fn new(command: PathBuf, args: Vec<OsString>, mut env: BTreeMap<String, String>) -> Self {
        for name in SECRET_ENV_NAMES {
            env.remove(*name);
        }
        Self { command, args, env }
    }

    pub(crate) fn from_parsed(
        command: PathBuf,
        args: Vec<OsString>,
        env: BTreeMap<String, String>,
    ) -> Self {
        Self { command, args, env }
    }

    pub fn command(&self) -> &Path {
        &self.command
    }

    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    pub fn env(&self) -> &BTreeMap<String, String> {
        &self.env
    }

    pub(crate) fn command_text(&self) -> Result<&str, InstallError> {
        unicode(self.command.as_os_str(), "Rust binary path")
    }

    pub(crate) fn argument_texts(&self) -> Result<Vec<&str>, InstallError> {
        self.args
            .iter()
            .map(|argument| unicode(argument, "MCP command argument"))
            .collect()
    }
}

impl fmt::Debug for ClientLaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientLaunchSpec")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

pub(crate) fn is_secret_environment_name(name: &str) -> bool {
    SECRET_ENV_NAMES.contains(&name)
}

fn unicode<'a>(value: &'a OsStr, context: &'static str) -> Result<&'a str, InstallError> {
    value
        .to_str()
        .ok_or(InstallError::NonUnicodePath { context })
}
