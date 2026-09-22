use clap::{Args, Subcommand};
use dg_xch_core::consensus::chain_definition::{ChainDefinition, ChainSelection};
use dg_xch_servers::chain_config::{initialize, read_selection};
use std::io::{Error, ErrorKind};
use std::path::PathBuf;

#[derive(Debug, Args)]
pub struct ChainArgs {
    #[command(subcommand)]
    command: ChainCommand,
}

impl ChainArgs {
    pub(crate) fn inherit_network(&mut self, root_network: Option<&str>) -> Result<(), Error> {
        match &mut self.command {
            ChainCommand::Init {
                development: true, ..
            } if root_network.is_some() => Err(Error::new(
                ErrorKind::InvalidInput,
                "--network conflicts with --development",
            )),
            ChainCommand::Init { network, .. } | ChainCommand::Inspect { network, .. } => {
                crate::inherit_network(network, root_network)
            }
        }
    }
}

#[derive(Debug, Subcommand)]
enum ChainCommand {
    Init {
        #[arg(long)]
        output: PathBuf,
        #[arg(long, conflicts_with = "development")]
        network: Option<String>,
        #[arg(long, conflicts_with = "development")]
        chain_config: Option<PathBuf>,
        #[arg(long)]
        development: bool,
        #[arg(long, requires = "development")]
        genesis_seed: Option<String>,
    },
    Inspect {
        #[arg(long)]
        network: Option<String>,
        #[arg(long)]
        chain_config: Option<PathBuf>,
    },
}

pub(crate) fn selection(
    network: Option<&str>,
    path: Option<&std::path::Path>,
) -> Result<ChainSelection, Error> {
    let selected = match path {
        Some(path) => read_selection(path)?,
        None => ChainSelection::from_config(network.unwrap_or("mainnet"), None)
            .map_err(|error| Error::new(ErrorKind::InvalidInput, error))?,
    };
    let resolved = selected.resolve().map_err(Error::other)?;
    if network.is_some_and(|network| network != resolved.network_id) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "--network does not match --chain-config",
        ));
    }
    Ok(selected)
}

fn print_chain(selection: &ChainSelection) -> Result<(), Error> {
    #[derive(serde::Serialize)]
    struct ChainInfo {
        network_id: String,
        handshake_network_id: String,
        allows_bootstrap: bool,
        constants: dg_xch_core::consensus::constants::ConsensusConstants,
    }
    let chain = selection.resolve().map_err(Error::other)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&ChainInfo {
            network_id: chain.network_id,
            handshake_network_id: chain.handshake_network_id,
            allows_bootstrap: chain.allows_bootstrap,
            constants: chain.constants,
        })
        .map_err(Error::other)?
    );
    Ok(())
}

pub fn run(args: ChainArgs) -> Result<(), Error> {
    match args.command {
        ChainCommand::Inspect {
            network,
            chain_config,
        } => print_chain(&selection(network.as_deref(), chain_config.as_deref())?),
        ChainCommand::Init {
            output,
            network,
            chain_config,
            development,
            genesis_seed,
        } => {
            let manifest = output.join("chain.json");
            let selected = if development {
                if manifest.try_exists()? && genesis_seed.is_none() {
                    let existing = read_selection(&manifest)?;
                    if !matches!(&existing, ChainSelection::Custom(definition) if definition.consensus.as_ref().is_some_and(|parameters| parameters.development))
                    {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "existing directory is not a development chain",
                        ));
                    }
                    existing
                } else {
                    ChainSelection::Custom(ChainDefinition::development(
                        genesis_seed.unwrap_or_else(|| {
                            format!("dg_xch/dev/{}", hex::encode(rand::random::<[u8; 32]>()))
                        }),
                    ))
                }
            } else {
                selection(network.as_deref(), chain_config.as_deref())?
            };
            initialize(&output, &selected)?;
            print_chain(&selected)?;
            eprintln!(
                "Initialized {}. No blocks or wallet keys were created.",
                output.display()
            );
            eprintln!(
                "Start the node with --chain-config {} --ssl-dir {} --db sqlite://{}",
                manifest.display(),
                output.join("ssl").display(),
                output.join("data/chain.db").display()
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::cli::{Cli, RootCommands};
    use clap::Parser;

    fn arguments(arguments: &[&str]) -> Result<ChainArgs, Error> {
        let mut cli = Cli::try_parse_from(arguments).map_err(Error::other)?;
        crate::apply_network(&mut cli)?;
        match cli.action {
            RootCommands::Chain(args) => Ok(args),
            _ => Err(Error::other("expected chain command")),
        }
    }

    #[test]
    fn root_network_reaches_chain_init_and_inspect() {
        for command in [
            vec!["dg", "--network", "testnet11", "chain", "inspect"],
            vec![
                "dg",
                "--network",
                "testnet11",
                "chain",
                "init",
                "--output",
                "unused",
            ],
        ] {
            let args = arguments(&command).unwrap();
            let network = match args.command {
                ChainCommand::Init { network, .. } | ChainCommand::Inspect { network, .. } => {
                    network
                }
            };
            assert_eq!(network.as_deref(), Some("testnet11"));
        }
        assert!(
            arguments(&[
                "dg",
                "--network",
                "mainnet",
                "chain",
                "inspect",
                "--network",
                "dgx"
            ])
            .is_err()
        );
        assert!(
            arguments(&[
                "dg",
                "--network",
                "mainnet",
                "chain",
                "init",
                "--output",
                "unused",
                "--development"
            ])
            .is_err()
        );
    }

    #[test]
    fn inherited_custom_network_is_checked_against_its_manifest_not_builtin_names() {
        let mut definition = ChainDefinition::development("cli network fixture".into());
        definition.network_id = "custom-cli-chain".into();
        let expected = ChainSelection::Custom(definition);
        let path = std::env::temp_dir().join(format!(
            "dgx-cli-chain-{}.json",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        dg_xch_servers::chain_config::write_new(&path, &serde_json::to_vec(&expected).unwrap())
            .unwrap();
        let args = arguments(&[
            "dg",
            "--network",
            "custom-cli-chain",
            "chain",
            "inspect",
            "--chain-config",
            path.to_str().unwrap(),
        ]);
        let selected = args.and_then(|args| match args.command {
            ChainCommand::Inspect {
                network,
                chain_config,
            } => selection(network.as_deref(), chain_config.as_deref()),
            _ => Err(Error::other("expected inspect command")),
        });
        let mismatch = selection(Some("mainnet"), Some(&path));
        std::fs::remove_file(path).unwrap();
        assert_eq!(selected.unwrap(), expected);
        assert!(mismatch.is_err());
    }
}
