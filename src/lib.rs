//! fireshare: send files between machines over plain HTTP on one port.
//!
//! One binary is both ends. `serve` shares a directory: a CLI client, or any
//! browser, can list it, download from it and upload into it. Every request
//! carries a shared token, and every upload is checked by SHA-256 and lands
//! atomically, so a broken transfer never leaves a half-written file behind.

pub mod client;
pub mod paths;
pub mod server;

use serde::{Deserialize, Serialize};

/// Header carrying the hex SHA-256 of a file's content, both ways.
pub const SHA256_HEADER: &str = "x-content-sha256";

/// One shared file, as listed by `GET /api/files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the shared directory, always with `/` separators.
    pub path: String,
    pub size: u64,
    /// Last modification, seconds since the Unix epoch.
    pub modified: u64,
}

/// What the server answers after storing an upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// A random token, hex encoded, for when `serve` is not given one.
pub fn random_token() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("the OS random source is available");
    hex::encode(buf)
}
