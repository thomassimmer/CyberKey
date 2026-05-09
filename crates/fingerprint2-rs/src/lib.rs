//! `fingerprint2-rs` — `no_std` UART driver for the M5Stack Unit Fingerprint2 (U203).
//!
//! # Overview
//!
//! This crate provides a blocking, `no_std`-compatible driver for the M5Stack
//! Fingerprint2 sensor (internal MCU: STM32G031G8U6). The driver is generic over
//! any UART implementation that satisfies the [`embedded_hal_nb::serial::Read`] and
//! [`embedded_hal_nb::serial::Write`] traits, making it usable both in firmware
//! (with the real `esp-idf-hal` UART peripheral) and in unit tests on the desktop
//! (with a `MockUart` backed by in-memory buffers).
//!
//! # `no_std` design
//!
//! This crate is compiled without the standard library in production (firmware).
//! When `cargo test` runs on the host, the test harness automatically re-enables
//! `std`, so all unit tests execute on the desktop with zero hardware required.
//!
//! No heap allocator is needed: all data structures are stack-allocated via
//! [`heapless`].
//!
//! # Quick start
//!
//! ```ignore
//! use fingerprint2_rs::Fingerprint2Driver;
//!
//! let mut driver = Fingerprint2Driver::new(uart, delay);
//! driver.handshake()?;
//! let (page_id, score) = driver.auto_identify(3)?;
//! ```
#![cfg_attr(not(test), no_std)]

pub mod commands;
pub mod driver;
pub mod error;
pub mod packet;

pub use driver::Fingerprint2Driver;
pub use error::FingerprintError;
