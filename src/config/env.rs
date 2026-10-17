use std::{collections::BTreeMap, env::VarError, fmt};

use thiserror::Error;

use crate::credentials::{
    CredentialInteraction, CredentialReader, DEFAULT_CREDENTIAL_ACCOUNT,
    DEFAULT_CREDENTIAL_SERVICE, SystemCredentials,
};

#[derive(Clone, Default, Eq, PartialEq)]
pub struct EnvironmentSnapshot {
    values: BTreeMap<String, String>,
    credential_status: CredentialStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStatus {
    Environment,
    SystemStore,
    #[default]
    Missing,
    AccessRequired,
    Unavailable,
}

#[derive(Debug, Error)]
pub enum EnvironmentError {
    #[error("环境变量 {name} 不是有效 UTF-8")]
    InvalidUnicode { name: &'static str },
}

impl EnvironmentSnapshot {
    pub fn capture(names: &'static [&'static str]) -> Result<Self, EnvironmentError> {
        let mut values = BTreeMap::new();
        for &name in names {
            match std::env::var(name) {
                Ok(value) => {
                    values.insert(name.to_owned(), value);
                }
                Err(VarError::NotPresent) => {}
                Err(VarError::NotUnicode(_)) => {
                    return Err(EnvironmentError::InvalidUnicode { name });
                }
            }
        }
        Ok(Self::from_map(values))
    }

    pub fn from_map(values: BTreeMap<String, String>) -> Self {
        let credential_status = if values
            .get("MICU_API_KEY")
            .is_some_and(|value| !value.trim().is_empty())
        {
            CredentialStatus::Environment
        } else {
            CredentialStatus::Missing
        };
        Self {
            values,
            credential_status,
        }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        let name = name.into();
        let value = value.into();
        if name == "MICU_API_KEY" {
            self.credential_status = if value.trim().is_empty() {
                CredentialStatus::Missing
            } else {
                CredentialStatus::Environment
            };
        }
        self.values.insert(name, value);
    }

    pub fn load_platform_secrets(&mut self) {
        self.load_platform_secrets_with(&SystemCredentials);
    }

    pub fn load_platform_secrets_with(&mut self, reader: &dyn CredentialReader) {
        if self
            .values
            .get("MICU_API_KEY")
            .is_some_and(|value| !value.trim().is_empty())
        {
            self.credential_status = CredentialStatus::Environment;
            return;
        }
        let service = self
            .values
            .get("MICU_KEYCHAIN_SERVICE")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CREDENTIAL_SERVICE);
        let account = self
            .values
            .get("MICU_KEYCHAIN_ACCOUNT")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_CREDENTIAL_ACCOUNT);
        self.credential_status =
            match reader.read(service, account, CredentialInteraction::Forbidden) {
                Ok(Some(secret)) if !secret.trim().is_empty() => {
                    self.values.insert("MICU_API_KEY".into(), secret);
                    CredentialStatus::SystemStore
                }
                Ok(_) => CredentialStatus::Missing,
                Err(crate::credentials::CredentialStoreError::AccessRequired) => {
                    CredentialStatus::AccessRequired
                }
                Err(_) => CredentialStatus::Unavailable,
            };
    }

    pub fn credential_status(&self) -> CredentialStatus {
        self.credential_status
    }
}

impl fmt::Debug for EnvironmentSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvironmentSnapshot")
            .field("keys", &self.values.keys().collect::<Vec<_>>())
            .finish()
    }
}
