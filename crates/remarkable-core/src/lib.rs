//! # remarkable-core
//!
//! The library half of `remarkable-mcp`: a reMarkable cloud client and document
//! model, with no MCP/transport concerns. Per the rust-cli-aspects lib/bin split,
//! this crate holds the real product; the `remarkable-mcp` binary is a thin wrapper.
//!
//! ## What's here
//!
//! * [`auth`] — token storage and the device/user token state machine.
//! * [`client`] — the [`CloudClient`], implementing the sync v3 metadata protocol
//!   with root-hash change detection and bounded parallel fetch.
//! * [`model`] — the [`Item`] document model.
//! * [`library`] — the [`Library`], reconstructing hierarchy (paths, trees,
//!   folders, search) from the flat cloud listing.
//!
//! ## Provenance
//!
//! The protocol and model are a Rust port that takes the best parts of three
//! reference servers: lanej (Go — sync protocol, path/tree logic, response
//! budgeting), wavyrai (Python — root-hash caching, parallel fetch, structured
//! errors), and SamMorrowDrums (Python — UX/safety ideas). See `docs/sources/`.

pub mod auth;
pub mod client;
pub mod error;
pub mod library;
pub mod model;

pub use auth::{default_token_path, TokenStore};
pub use client::{AuthStatus, ClientConfig, CloudClient};
pub use error::{Error, Result};
pub use library::{Library, TreeNode};
pub use model::{Item, ItemType};
