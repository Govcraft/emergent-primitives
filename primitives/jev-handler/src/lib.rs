//! Jev Handler — ask TypeSafe System One typed questions about event payloads.
//!
//! One irreducible I/O act per message: a single POST to the System One
//! evaluation endpoint asking a fixed set of typed questions about the inbound
//! payload. Everything around it — parsing the questions file, selecting the
//! state, building the body, checking the answers, deciding a retry, assembling
//! a payload — is a pure function, so nearly all of this crate is testable with
//! no network at all.
//!
//! The modules are arranged by that split: [`client`] and the binary's shell
//! perform I/O, and everything else is pure.

pub mod args;
pub mod client;
pub mod error;
pub mod payload;
pub mod questions;
pub mod request;
pub mod response;
pub mod retry;
