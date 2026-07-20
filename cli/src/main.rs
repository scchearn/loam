mod codegraph;
mod datecheck;
mod lint;
mod markdown;
mod memory;
mod state;

use std::env;
use std::process;

fn main() {
    process::exit(run(env::args().skip(1)));
}

fn run(mut args: impl Iterator<Item = String>) -> i32 {
    match args.next().as_deref() {
        Some("state") => state::run(args),
        Some("codegraph") => codegraph::run(args),
        Some("datecheck") => datecheck::run(args),
        Some("lint") => lint::run(args),
        _ => {
            usage();
            1
        }
    }
}

fn usage() {
    eprintln!(
        "Usage:\n  loam state [--fast] <workspace-root>\n  loam codegraph walk <codebase-root> [--exclusions <file>] [--summary] [--no-gitignore]\n  loam datecheck <check|fix> <wiki-root> [--offset +HH:MM]\n  loam lint [--only markdown|memory|work] <workspace-root> [--now 'YYYY-MM-DD HH:MM ±HH:MM']\n\n  lint runs all three domains by default; --only runs exactly one.\n  --now overrides the clock for date-relative rules; it exists for\n  deterministic tests and replay, not for routine use."
    );
}
