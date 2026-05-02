//! `cyberkey-core` — TOTP engine and BCD helpers for CyberKey.
//!
//! # `no_std` design
//!
//! This crate is compiled without the standard library in production (firmware).
//! When `cargo test` runs on the host, the test harness automatically re-enables
//! `std`, so all unit tests execute on the desktop with zero hardware required.
//!
//! No heap allocator is needed: all data structures are stack-allocated.
#![cfg_attr(not(test), no_std)]

pub mod bcd;
pub mod error;
pub mod totp;

pub use error::TotpError;
pub use totp::generate_totp;
