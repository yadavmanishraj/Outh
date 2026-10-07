//! Outh core: a faithful Rust port of gotohp's `core` Go package.
//! See CONTRACT.md at the repository root for the frozen design contract.

pub mod api;
pub mod auth;
pub mod config;
pub mod create_media_items;
pub mod credential;
pub mod error;
pub mod filename;
pub mod http;
pub mod livephoto;
pub mod livephoto_metadata;
pub mod protocol;
pub mod sha1calc;
pub mod types;
pub mod upload;
pub mod wire;

pub use error::Error;
pub type Result<T> = std::result::Result<T, Error>;
