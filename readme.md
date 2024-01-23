DruidGarden XCH Utils
======
[![CI](https://github.com/GalactechsLLC/dg_xch_utils/actions/workflows/ci.yml/badge.svg)](https://github.com/GalactechsLLC/dg_xch_utils/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/dg_xch_cli.svg)](https://crates.io/crates/dg_xch_cli)

## Introduction
 A collection of packages for working with the Chia Blockchain using Rust,

- **dg_xch_cli** - Command line tools to work with nodes without having to write code
- **dg_xch_clients** - Library for Chia compatible RPC and Websocket Clients
- **dg_xch_core** - Library containing type definitions, CLVM tools, Consensus and Pool definitions
- **dg_xch_keys** - Library for creating keys and generating Mnemonics/Wallets
- **dg_xch_macros** - Derive Marcos for Chia Serialization
- **dg_xch_pos** - Proof of Space library for manipulating Chia Plot files
- **dg_xch_puzzles** - Library for working with CLVM puzzles
- **dg_xch_serialize** - Library defining ChiaSerialize and creating Impl for base types
- **dg_xch_servers** - Library for Chia compatible RPC and Websocket Servers

## Install
### Prerequisites
- Install Rust by following the instructions at https://www.rust-lang.org/tools/install

### From Cargo

```cargo install dg_xch_cli```

### From Source

```
git clone https://github.com/GalactechsLLC/dg_xch_utils.git
cd dg_xch_utils
cargo build --release
sudo cp target/release/dg_xch_cli /usr/local/bin/dg_xch_cli
```

> [!TIP]
> To Print all available commands ```dg_xch_cli --help``` <br>
> To Print Command Help ```dg_xch_cli <COMMAND> --help```
