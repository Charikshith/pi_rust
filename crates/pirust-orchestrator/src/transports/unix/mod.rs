//! Port of `packages/server/src/transports/unix/`.
//!
//! **Wave 5 scope decision (named, not silent — see `docs/analysis/
//! 04-orchestrator.md` §8 and `plan.md`'s Wave 5 entry):** real Pi's
//! `listener.ts` binds a genuine POSIX Unix-domain socket file (inode
//! identity via `lstat`, `link`-based atomic bind-then-publish, `chmod`
//! permissions, ENOENT/dev+ino stale-socket probing). `tokio::net::
//! UnixListener` — the direct Rust equivalent — is `#[cfg(unix)]`-gated and
//! does not exist on Windows, which is this project's own dev machine.
//!
//! The plan's own instruction was to try a cross-platform local-socket crate
//! (`interprocess`) first and verify directly rather than assume. That was
//! evaluated and NOT adopted: `interprocess`'s Windows backend is a named
//! pipe, not a real `AF_UNIX` filesystem socket — it has no inode identity,
//! no `lstat`/`link`/`chmod` semantics, and no filesystem path collision
//! behavior at all. Substituting it as the *only* implementation would not
//! let this dev machine verify the actual thing `listener.ts` does any
//! better than not using it — it would just add a second, non-matching
//! transport and a new dependency for no verification benefit, while
//! silently diverging from Pi's real semantics on every platform it runs
//! (real Unix domain sockets on Unix are also possible with `interprocess`,
//! but `tokio::net::UnixListener` already does that natively with zero
//! extra dependency).
//!
//! Resolution actually shipped this wave:
//! - [`options`]: every pure, non-I/O piece of `listener.ts` (path
//!   validation, option resolution/defaulting, the owned-bind-path SHA-256
//!   hash) — no `#[cfg(unix)]` gate, since none of it touches an OS socket.
//!   Compiles and is unit-tested on this Windows dev machine today.
//! - `listener` (`#[cfg(unix)]`): the real, faithful 1:1 port of
//!   `UnixListener`/`UnixByteConnection` against `tokio::net::UnixListener`/
//!   `UnixStream` — bind-then-link, stale-socket cleanup, backpressure,
//!   graceful close. **Verified two ways on this Windows dev machine, one
//!   way NOT:** `cargo check`/`cargo clippy --all-targets -D warnings`
//!   against a cross-compiled `x86_64-unknown-linux-gnu` target (added via
//!   `rustup target add`) both pass clean — real type/borrow-checker/lint
//!   verification of this exact code, not just "it looks right." What is
//!   genuinely **UNVERIFIED ON THIS DEV MACHINE** is *running* it — a
//!   cross-compiled Linux binary cannot execute on Windows, so `tests/
//!   unix_transport.rs`'s actual pass/fail has not been observed here
//!   (named, not silent — same precedent as feat-006's `native_modifiers.rs`
//!   win32/darwin split and feat-012's `#[cfg(unix)]`-only SIGTERM/SIGHUP
//!   handling). It will compile, lint, AND run wherever this crate is built
//!   on real Linux/macOS/CI.
//! - The protocol-conformance layer that `test/conformance.test.ts` actually
//!   cares about (hello/version negotiation, fragmented hello, handshake
//!   timeout, oversized frames, request/response ordering, snapshot
//!   catch-up, terminal-error disconnects, graceful close) is transport-
//!   agnostic — it only needs SOME real, async, ordered byte-stream
//!   transport, not specifically a Unix domain socket. `crate::testing::
//!   duplex` provides an in-memory `tokio::io::duplex`-backed
//!   [`crate::listener::PiServerListener`]/[`crate::connection::
//!   ByteConnection`] pair that drives the SAME `PiServer`/
//!   `ProtocolTestClient` code over real async I/O, cross-platform — this
//!   IS verified on this dev machine (`tests/conformance.rs`).

pub mod options;

#[cfg(unix)]
mod listener;

#[cfg(unix)]
pub use listener::{create_unix_listener, UnixByteConnection};

pub use options::{
    get_owned_bind_path, resolve_unix_listener_options, validate_unix_socket_path,
    ResolvedUnixListenerOptions, UnixListenerConfigError, UnixListenerOptions,
    MAX_UNIX_SOCKET_PATH_BYTES,
};
