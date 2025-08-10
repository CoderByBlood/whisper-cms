// crates/infra/src/db/mod.rs
pub mod conn;
pub mod migrate;
pub mod seed;
pub mod health;

// Re-export a clean API
pub use conn::{connect, Conn};