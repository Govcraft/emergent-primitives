//! Stream Runner internals.
//!
//! The binary is a thin async shell around [`machine`], which holds every rule
//! this primitive has and holds them as a pure function. Anything that decides
//! belongs in the machine; anything that talks to the engine, the clock, or the
//! log belongs in `main.rs`.

pub mod machine;
