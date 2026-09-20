//! HTTP Source: webhook receiver for Emergent.
//!
//! Receives HTTP requests and publishes one `http.request` event per request,
//! with optional HMAC-SHA256 signature validation.
//!
//! Sources are SILENT: they only produce domain messages. All lifecycle events
//! are published by the engine.
//!
//! # Shape
//!
//! One request handler performs the only I/O act in the crate, the publish.
//! Everything it decides is a pure function it calls: [`addr`] answers "which
//! address is the caller", [`payload`] builds the published JSON,
//! [`signature`] checks the HMAC, and [`route_path`] answers "is `--path` a
//! route the router accepts" before the router is asked. That split is what makes the interesting
//! behaviour testable without a socket or an engine.

pub mod addr;
pub mod app;
pub mod args;
pub mod payload;
pub mod route_path;
pub mod signature;
