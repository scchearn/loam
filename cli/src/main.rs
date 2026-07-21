mod check;
mod checkpoint;
mod codegraph;
mod datecheck;
mod json;
mod lint;
mod markdown;
mod memory;
mod sha256;
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
        Some("checkpoint") => checkpoint::run(args),
        Some("check") => check::run(args),
        _ => {
            usage();
            1
        }
    }
}

fn usage() {
    eprintln!(
        "Usage:\n  loam state [--fast] <workspace-root>\n  loam codegraph index <wiki-root> [--codebase-root <codebase-root>]\n  loam codegraph walk <codebase-root> [--exclusions <file>] [--summary] [--no-gitignore]\n  loam codegraph diff <codebase-root> [<wiki-root>] [--exclusions <file>] [--no-gitignore] [--strict]\n  loam datecheck <check|fix> <wiki-root> [--offset +HH:MM]\n  loam lint [--only markdown|memory|work] <workspace-root> [--now 'YYYY-MM-DD HH:MM ±HH:MM']\n  loam checkpoint verify <note.md>\n  loam checkpoint state [--window <minutes>] [<workspace-root>]\n  loam check versions <repo-root> [--plugin | --runtime]\n\n  lint runs all three domains by default; --only runs exactly one.\n  --now overrides the clock for date-relative rules; it exists for\n  deterministic tests and replay, not for routine use.\n  checkpoint verify always exits 0: it must never block a save.\n  check versions is offline and asserts one version within each domain:\n  plugin (released as v<version>) and runtime (released as cli-v<version>).\n  The two are independent and are never compared to each other."
    );
}
