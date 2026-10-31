//! `build-xtask` - workspace automation for the Gateway repository.
//!
//! ## Invariants
//!
//! - Tier: tooling. The architecture checks run as tests
//!   (`cargo test -p build-xtask`); `cargo xtask tidy` prints the same
//!   report on demand.

mod debug_spelling;
mod manifest;
mod no_doctests;
mod tidy;
mod unsafe_allowlist;
mod workspace;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) else {
        eprintln!("error: cannot locate workspace root from CARGO_MANIFEST_DIR");
        return ExitCode::FAILURE;
    };
    match args.get(1).map(String::as_str) {
        Some("tidy") => {
            let violations = tidy::all_violations(root);
            if violations.is_empty() {
                println!("tidy: no violations");
                ExitCode::SUCCESS
            } else {
                for violation in &violations {
                    eprintln!("tidy: {violation}");
                }
                ExitCode::FAILURE
            }
        }
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: cargo xtask tidy");
    ExitCode::from(2)
}
