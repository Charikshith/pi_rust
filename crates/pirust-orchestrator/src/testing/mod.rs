//! Test-only reference doubles, port of `packages/server/src/testing/`.
//! Only `service.ts` (`TestServerService`/`TestSessionRuntime`) is ported
//! this wave — `client.ts`/`server.ts` (a `ProtocolTestClient` and an
//! in-memory transport pair) are Wave 5 (Unix transport) concerns per
//! `plan.md`, since nothing before that wave needs a real byte-transport
//! test double.

pub mod service;
