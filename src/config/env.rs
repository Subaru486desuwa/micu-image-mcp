use std::{collections::BTreeMap, env::VarError, fmt};

use thiserror::Error;

use crate::credentials::{DEFAULT_CREDENTIAL_ACCOUNT, DEFAULT_CREDENTIAL_SERVICE, load_api_key};

#[derive(Clone, Default, Eq, PartialEq)]
pub struct EnvironmentSnapshot {
    values: BTreeMap<String, String>,
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
        Ok(Self { values })
    }

    pub fn from_map(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.values
    }

    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.values.insert(name.into(), value.into());
    }

    pub fn load_platform_secrets(&mut self) {
        if self
            .values
            .get("MICU_API_KEY")
            .is_some_and(|value| !value.trim().is_empty())
        {
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
        if let Ok(Some(secret)) = load_api_key(service, account)
            && !secret.trim().is_empty()
        {
            self.values.insert("MICU_API_KEY".into(), secret);
        }
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
