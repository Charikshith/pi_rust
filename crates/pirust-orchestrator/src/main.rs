//! `pirust-orchestrator` — pirust port of `@earendil-works/pi-server` (a
//! transport-neutral, multi-session-multiplexing server library — see
//! `docs/analysis/04-orchestrator.md`, rewritten 2026-08-23 after real Pi
//! renamed and redesigned this package away from process-spawning).
//!
//! feat-009 Wave 1 (CBOR + framing codec) is done; the `PiServer` state
//! machine, Unix transport, and a runnable binary land in later waves.

fn main() {
    eprintln!(
        "pirust-orchestrator: not yet implemented (scaffold — Wave 1 codec only). See docs/analysis/04-orchestrator.md"
    );
    std::process::exit(2);
}
