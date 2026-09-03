use clap::Parser;

/// Terminal-native remote workspace runtime for Claude Code.
#[derive(Parser)]
#[command(name = "ccnm", version = ccnm_core::VERSION)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
