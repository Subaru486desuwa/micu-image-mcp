//! Offline process and HTTP fixtures shared by behavioral tests and engineering tools.

pub mod audit;
pub mod benchmark;
pub mod clients;
pub mod engineering;
pub mod harness;
pub mod live;
pub mod mock;
pub mod repository;
pub mod stdio;

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;
