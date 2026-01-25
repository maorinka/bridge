//! Bridge Common - Shared types and protocol definitions
//!
//! This crate contains all the shared types used across the Bridge system:
//! - Protocol messages for video, audio, and input
//! - Configuration types
//! - Common error types

pub mod protocol;
pub mod types;
pub mod error;

pub use protocol::*;
pub use types::*;
pub use error::*;
