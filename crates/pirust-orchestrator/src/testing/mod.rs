//! Test-only reference doubles, port of `packages/server/src/testing/`.
//! `service.rs` (`TestServerService`/`TestSessionRuntime`) shipped in Wave
//! 4b. Wave 5 adds `client.rs` (`ProtocolTestClient`, port of `client.ts`)
//! and `duplex.rs` (a cross-platform in-memory transport double, NOT a Pi
//! port — see its own module doc) so the protocol-conformance battery can
//! be driven over a real byte transport on any platform, including this
//! project's Windows dev machine where `tokio::net::UnixListener` doesn't
//! exist.

pub mod client;
pub mod duplex;
pub mod service;
