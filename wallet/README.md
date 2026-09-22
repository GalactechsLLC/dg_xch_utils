# dg_xch_wallet

Wallet accounts, standard-coin signing, trusted full-node synchronization, and persistent local state. This is a Rust library; the [native desktop application](../gui/README.md) provides its user interface.

## Status

Wallet sessions use a file-backed SQLite database, not an in-memory-only balance cache. Accounts can remain unlocked independently, with separate background synchronization. The database retains discovered coins, the last complete chain snapshot, address derivation progress, signed transactions, broadcast results, and input reservations across restarts.

Standard payments are implemented. CATs, NFTs, offers, hardware signing, automatic pending-transaction recovery, and a standalone wallet daemon are not complete. Use test funds until funded-chain, reorganization, and recovery testing has been completed.

## Integration

From the repository root:

```sh
cargo build -p dg_xch_cli
./target/debug/dgx init
./target/debug/dgx gui
```

1. Configure the full-node RPC hostname, port, client certificate, client key, and trusted private CA in Settings. The certificate must match the hostname; verification is not bypassed.
2. Select Chia mainnet or Chia testnet11; the desktop supplies their pinned genesis block header hashes. Only Custom requires a chain definition and an independently verified genesis block **header hash**, not the genesis challenge.
3. Use a full node built with `coin-index` enabled. The wallet queries spent and unspent coins by puzzle hash and refuses to synchronize against a different genesis or an unsynchronized node.
4. Import or create an account in Wallets, retain an independent mnemonic backup, and choose a password of at least 12 bytes.
5. Unlock one or more accounts. Previously saved balances and history are available immediately, even before the node responds. They are cached observations, not proof of current spendability. Sending requires a fresh successful scan.

Applications embedding this library call the asynchronous `accounts::WalletSession::new` constructor with a secret key, verified `FullnodeClient`, consensus constants, expected genesis header hash, and database path. `sync`, `send`, `snapshot`, `transactions`, and `checkpoint` provide the session API. `MemoryWallet` remains an internal signing/cache implementation and a compatibility API; it is not the desktop wallet's persistence layer.

## Configuration and storage

New Linux desktop profiles use `~/.dgx/config` and `~/.dgx/data`. macOS and Windows use native per-user directories through `directories::ProjectDirs` with organization `Galactechs` and application `dg_xch`. Existing initialized profiles remain discoverable; no wallet files are moved automatically. `dgx init` can select custom paths, recorded in the shared application profile. Settings displays the resolved paths. Within the data directory:

```text
accounts/<account-id>.json
wallets/<account-id>/<genesis-header-hash>.sqlite
wallets/<account-id>/<genesis-header-hash>.sqlite-wal
wallets/<account-id>/<genesis-header-hash>.sqlite-shm
wallets/<account-id>/<genesis-header-hash>.sqlite.lock
```

The encrypted account JSON contains the master key protected by Argon2id and XChaCha20-Poly1305. Identity, name, network, and format version are authenticated. Passwords and private keys are not stored in SQLite. The database contains public-chain information and signed transactions, which still reveal wallet activity and should be treated as private. SQLite itself is not encrypted.

SQLite uses WAL journaling, `synchronous=FULL`, foreign-key checks, schema versioning, and an exclusive per-wallet application lock. The database binds itself to both the account's public-key-derived identity and the genesis header hash. Coin state, derivation progress, transaction history, and reservations commit together. Transaction inputs are durably reserved before a network submission is attempted. Unix database files are restricted to mode `0600` and their wallet directory to `0700`; other platforms rely on the user's profile-directory access controls.

Each scan replaces the previous complete set of coin records after checking that the node's peak has not changed during the scan. A failed scan does not erase the last good balance. Spent input reservations are retained so a reorganization cannot silently make an already-signed transaction's coins spendable again.

## Migration, backup, and recovery

The first open imports the earlier `<genesis-header-hash>.json` derivation/pending journal if there is no committed SQLite state. The original journal is left untouched. Subsequent opens use SQLite and do not re-import the old journal. Unknown/newer database schema versions and mismatched identities fail closed rather than being overwritten.

Keep the mnemonic backup separately from the computer. Back up the encrypted account files and the wallet database together: the mnemonic restores keys but does not restore local transaction reservations, labels, or broadcast history.

For a file-copy backup, stop the desktop and any other wallet process first, then copy the account directory and the entire relevant wallet directory, including any `-wal` and `-shm` files still present. Do not copy only a live `.sqlite` file or delete its WAL. Embedded applications can call `WalletSession::checkpoint` before closing; SQLite's online backup facilities are preferable for backups while a wallet is active.

Restore into a stopped application using the same account and genesis identity. Preserve the original files before attempting repairs. A missing or corrupted database should not be silently deleted to unlock funds: an earlier signed transaction may still be pending elsewhere. Rejected or ambiguous broadcasts retain their input reservations and currently require investigation rather than automatic release or retry.

## Validation

```sh
cargo test -p dg_xch_wallet
cargo test -p dg_xch_wallet --test sqlite_wallet
cargo clippy -p dg_xch_wallet --all-targets -- -D warnings
```

The SQLite tests cover restart restoration without RPC, exact large balances, transaction rollback, account/network isolation, schema rejection, journal migration, durable reservations, replacement of reorged snapshots, and Unix file permissions. They do not replace end-to-end funded-chain or crash/power-loss testing.

## Limitations

- Synchronization trusts the configured TLS-authenticated full node; this is not a light-wallet consensus verifier.
- Discovery scans hardened and unhardened addresses with a gap of 20 and a limit of 100,000 derivations. A limit hit is an error, not a partial balance.
- Coin history is currently fully refreshed and rewritten transactionally; very large wallets need incremental synchronization and pagination work.
- A transaction whose inputs are spent is not necessarily the transaction that spent them. The persisted `inputs_spent` flag is not a confirmation proof.
- Generic PlotNFT spending returns an explicit unsupported error. Existing travel helpers are library functionality, not a complete desktop pooling workflow.

See the [repository README](../readme.md) for the other services and development setup.
