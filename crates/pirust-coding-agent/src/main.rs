//! `pirust` — the interactive coding-agent CLI.
//!
//! pirust port of `packages/coding-agent` (`@earendil-works/pi-coding-agent`). See
//! `docs/analysis/03-coding-agent.md`. Real CLI/bootstrap lands in feat-005;
//! interactive mode + extensions in feat-007.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("pirust (scaffold) — port in progress. See docs/analysis/00-overview.md");
        return;
    }
    // Pi's short form for version is `-v`, NOT `-V` (`cli/args.ts`); `-V` would be a
    // divergence. The real parser lands with feat-005's args module.
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("pirust {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    eprintln!(
        "pirust: not yet implemented (scaffold). crates linked: {}, {}, {}, {}.",
        pirust_ai::name(),
        pirust_agent_core::name(),
        pirust_tui::name(),
        pirust_tools::name(),
    );
    std::process::exit(2);
}
