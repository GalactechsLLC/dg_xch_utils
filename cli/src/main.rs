pub mod cli;
pub mod commands;
pub mod wallet_commands;

use clap::Parser;
use cli::*;
use simple_logger::SimpleLogger;
use std::io::Error;
use wallet_commands::create_cold_wallet;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let cli = Cli::parse();
    SimpleLogger::new().env().init().unwrap_or_default();

    match cli.action {
        RootCommands::GetCoinRecord { .. } => {
            //Do Stuff Here
        }
        RootCommands::CreateWallet { action } => match action {
            WalletAction::WithNFT { .. } => {}
            WalletAction::Cold => create_cold_wallet()?,
        },
    }
    Ok(())
}
