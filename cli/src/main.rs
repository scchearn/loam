use std::env;
use std::process;

fn main() {
    process::exit(loam::run(env::args().skip(1)));
}
