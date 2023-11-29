use {clap::Parser, solana_sdk::clock::Slot};

#[derive(Debug, Parser)]
struct Cli {
    /// Slot to fetch block and perform priority checks for.
    slot: Slot,
}

fn main() {
    let cli = Cli::parse();
    println!("Args: {cli:?}");
}
