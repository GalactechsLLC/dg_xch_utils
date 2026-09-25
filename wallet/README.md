# dg_xch_wallet

Chia wallet accounts, transaction signing, and synchronization through a trusted full node. Encrypted account files hold keys; SQLite stores wallet state across restarts.

## Try it in the desktop

From the repository root:

```sh
cargo install --path cli --locked
dgx init
dgx gui
```

The default network is Chia mainnet. Connect to a synchronized node with wallet coin, hint, and puzzle/solution RPCs. The default `dgx` node build includes those indexes.

Create or import an account in **Wallets**, save its recovery phrase separately, and choose a password of at least 12 bytes. Multiple accounts can stay unlocked. Cached balances load immediately, but sending requires a fresh successful scan.

## Integration

Use `accounts::WalletSession::new` with a secret key, verified `FullnodeClient`, consensus constants, trusted genesis **header hash**, and database path. The main methods are `sync`, `send`, `snapshot`, `transactions`, and `checkpoint`.

Sessions persist coins, derivation progress, signed transactions, and input reservations before broadcasting. A failed scan keeps the last good snapshot. This is trusted-node synchronization, not independent light-client consensus verification.

## Asset API

`WalletSession::asset_transaction(action, fee)` supports:

| Asset | Support |
| --- | --- |
| XCH | Standard payments |
| CAT1 | Read-only discovery |
| CAT2 | Fixed-supply issuance and multi-coin transfers |
| NFT1 | Minting and transfers |
| Chia DID1 | Creation and transfers with `DidType::Cni` |
| JuliaDID | Enum placeholder only; spending returns an unsupported error |

`watch_cat(asset_id)` adds discovery for older unhinted CAT coins. CAT amounts are integer base units (1 CAT = 1,000 units); XCH fees are mojos. NFTs and DIDs are indivisible. NFT metadata is untrusted text and is not downloaded.

`create_offer`, `take_offer`, and `cancel_offer` support XCH/CAT2 offers. `offers::review` inspects terms, not current spendability. Signed offers and reservations survive restarts. Cancellation is only effective after its self-spend confirms; CAT-only cancellation currently needs zero fee unless the original inputs include enough XCH.

NFT offers, restricted CATs, NFT0, DID social recovery, hardware signing, and automatic ambiguous-transaction recovery are not supported. Use test funds while interoperability and recovery testing continues.

## Storage and backups

Within the initialized data directory:

```text
accounts/<account-id>.json
wallets/<account-id>/<genesis-header-hash>.sqlite
```

Account keys use Argon2id and XChaCha20-Poly1305 encryption. SQLite is **not encrypted** and contains private wallet activity, but no passwords or private keys. It uses WAL journaling, full synchronization, and account/network binding.

Keep the recovery phrase offline. To make a file-copy backup, close every wallet process and copy both the account files and complete wallet directories, including any remaining `-wal` and `-shm` files. A mnemonic alone does not restore labels or transaction reservations.

Do not delete the database to clear a pending transaction: an already-signed spend may still be accepted elsewhere. Preserve the originals before recovery work. Address and asset scans have explicit limits; large wallets may need pagination/incremental-sync improvements.

[Desktop](../gui/README.md) · [Keys](../keys/README.md) · [Puzzles](../puzzles/README.md)
