//! Real RPC wallet/asset acceptance for the two-farmer Falcon Compose stacks.
use dg_xch_clients::rpc::full_node::{FullnodeAPI, FullnodeClient};
use dg_xch_core::{
    blockchain::{
        coin_record::CoinRecord, sized_bytes::Bytes32, spend_bundle::SpendBundle,
        tx_status::TXStatus,
    },
    consensus::{
        block_generator::{additions_for_conditions, conditions_from_spend_bundle},
        constants::ConsensusConstants,
    },
    signatures::{SignatureSigner, falcon_keys::FalconKeyring, falcon_wallet},
};
use dg_xch_wallet::{
    assets::{AssetAction, AssetCoin, AssetKind, NftLaunch, build_asset_transaction},
    falcon::{FalconWallet, Payment, WalletSnapshot, combine},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::Error,
    path::Path,
    sync::Arc,
    time::Duration,
};

struct Scenario<'a> {
    clients: &'a [FullnodeClient],
    constants: &'a ConsensusConstants,
    transactions: Vec<Value>,
}

impl Scenario<'_> {
    fn zk(&self) -> bool {
        self.constants.falcon_aggregation
            == Some(dg_xch_core::signatures::labrador::LabradorProfile::Packed11ZkV2)
    }

    async fn finalize(&self, bundle: &SpendBundle) -> Result<SpendBundle, Error> {
        #[cfg(feature = "labrador")]
        if self.zk()
            && bundle
                .authorization
                .as_ref()
                .is_some_and(|a| a.labrador.is_none())
        {
            return dg_xch_wallet::falcon::prove_zk(
                bundle.clone(),
                self.constants,
                self.height().await? + 1,
            )
            .await;
        }
        Ok(bundle.clone())
    }
    async fn height(&self) -> Result<u32, Error> {
        Ok(self.clients[0]
            .get_blockchain_state()
            .await?
            .peak
            .ok_or_else(|| Error::other("waiting for genesis"))?
            .height)
    }

    async fn reject_corrupt(&self, bundle: &SpendBundle) -> Result<(), Error> {
        let mut corrupt = bundle.clone();
        let auth = corrupt
            .authorization
            .as_mut()
            .ok_or_else(|| Error::other("wallet test has no authorization"))?;
        if let Some(proof) = &mut auth.labrador {
            // The public nonce is transcript-bound; the envelope stays well formed.
            let index = if proof.starts_with(b"DGXLSET2") {
                proof.len() - 1
            } else {
                42
            };
            proof[index] ^= 1;
        } else {
            auth.transactions
                .first_mut()
                .ok_or_else(|| Error::other("missing signature"))?[100] ^= 1;
        }
        match self.clients[0].push_tx(&corrupt).await {
            Err(error)
                if error
                    .error
                    .as_deref()
                    .is_some_and(|e| e.contains("BadAggregateSignature")) =>
            {
                Ok(())
            }
            other => Err(Error::other(format!(
                "corrupt wallet signature was not explicitly rejected: {other:?}"
            ))),
        }
    }

    async fn confirm(
        &mut self,
        label: &str,
        bundle: &SpendBundle,
        submitter: usize,
    ) -> Result<u32, Error> {
        let bundle = self.finalize(bundle).await?;
        if self.zk() {
            self.reject_corrupt(&bundle).await?;
        }
        let bundle = &bundle;
        let conditions =
            conditions_from_spend_bundle(bundle, self.height().await? + 1, self.constants)
                .map_err(Error::other)?;
        let additions = additions_for_conditions(&conditions, &[]);
        let removal_ids: HashSet<_> = bundle.coin_spends.iter().map(|s| s.coin.name()).collect();
        let addition_ids: HashSet<_> = additions.iter().map(|c| c.name()).collect();
        let inputs: Vec<_> = bundle
            .coin_spends
            .iter()
            .map(|s| s.coin)
            .filter(|c| !addition_ids.contains(&c.name()))
            .collect();
        let outputs: Vec<_> = additions
            .into_iter()
            .filter(|c| !removal_ids.contains(&c.name()))
            .collect();
        if inputs.is_empty() || outputs.is_empty() {
            return Err(Error::other(
                "wallet scenario needs persistent inputs and outputs",
            ));
        }
        if self.clients[submitter].push_tx(bundle).await? == TXStatus::FAILED {
            return Err(Error::other(format!(
                "{label}: valid wallet transaction rejected"
            )));
        }
        eprintln!(
            "Wallet scenario: submitted {label} ({} Falcon signatures)",
            conditions.falcon_signatures.len()
        );
        // Keep the exact randomized signatures while the block prover works.
        let ids: Vec<_> = inputs.iter().chain(&outputs).map(|c| c.name()).collect();
        loop {
            let mut confirmed = None;
            let mut block_hash = None;
            let mut ready = true;
            for client in self.clients {
                let mut records = HashMap::new();
                for chunk in ids.chunks(128) {
                    records.extend(
                        client
                            .get_coin_records_by_names(chunk, Some(true), None, None)
                            .await?
                            .into_iter()
                            .map(|r| (r.coin.name(), r)),
                    );
                }
                if inputs
                    .iter()
                    .any(|c| records.get(&c.name()).is_none_or(|r| !r.spent))
                    || outputs.iter().any(|c| !records.contains_key(&c.name()))
                {
                    ready = false;
                    continue;
                }
                let height = records[&inputs[0].name()].spent_block_index;
                if inputs.iter().any(|c| {
                    records[&c.name()].coin != *c || records[&c.name()].spent_block_index != height
                }) || outputs.iter().any(|c| {
                    records[&c.name()].coin != *c
                        || records[&c.name()].spent
                        || records[&c.name()].confirmed_block_index != height
                }) {
                    return Err(Error::other(format!(
                        "{label}: confirmed coin state differs from wallet spend"
                    )));
                }
                let hash = client.get_block_record_by_height(height).await?.header_hash;
                if confirmed.is_some_and(|h| h != height) || block_hash.is_some_and(|h| h != hash) {
                    ready = false;
                    continue;
                }
                confirmed = Some(height);
                block_hash = Some(hash);
            }
            if ready {
                let height = confirmed.ok_or_else(|| Error::other("no confirming nodes"))?;
                #[allow(unused_mut)]
                let mut zk_stats: Option<(usize, usize, usize)> = None;
                #[cfg(feature = "labrador")]
                if self.zk() {
                    let submitted = bundle
                        .authorization
                        .as_ref()
                        .and_then(|a| a.labrador.as_ref())
                        .ok_or_else(|| {
                            Error::other("wallet submitted direct signatures on ZK acceptance path")
                        })?;
                    zk_stats = Some(dg_xch_core::signatures::labrador::evidence_stats(
                        conditions.falcon_signatures.len(),
                        submitted,
                    )?);
                    // No node needs the hidden signatures: the proof actually
                    // submitted by the wallet must survive inside every block.
                    for client in self.clients {
                        let record = client.get_block_record_by_height(height).await?;
                        let block = client.get_block(&record.header_hash).await?;
                        let proof = block
                            .authorization
                            .as_ref()
                            .and_then(|a| a.labrador.as_ref())
                            .ok_or_else(|| Error::other("block lost wallet proof"))?;
                        if proof != submitted {
                            return Err(Error::other("isolated wallet proof changed in block"));
                        }
                    }
                }
                self.transactions.push(json!({"operation":label,"bundle_id":bundle.name()?,"height":height,
                    "header_hash":block_hash,"inputs":inputs.len(),"outputs":outputs.len(),
                    "falcon_signatures":conditions.falcon_signatures.len(),
                    "fee":conditions.removal_amount-conditions.addition_amount,"verified_nodes":self.clients.len(),
                    "wallet_zk":zk_stats,"authorization_bytes":bundle.authorization.as_ref().and_then(|a| a.labrador.as_ref()).map(Vec::len)}));
                eprintln!("Wallet scenario: {label} confirmed on all nodes at height {height}");
                return Ok(height);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

fn funding(snapshot: &WalletSnapshot, needed: u64) -> Result<Vec<CoinRecord>, Error> {
    let mut candidates = snapshot.native.clone();
    candidates.sort_by_key(|r| (r.coin.amount, r.coin.name().const_bytes()));
    let mut result = Vec::new();
    let mut total = 0u128;
    for record in candidates {
        if total >= u128::from(needed) {
            break;
        }
        if !record.spent {
            total += u128::from(record.coin.amount);
            result.push(record);
        }
    }
    if total < u128::from(needed) {
        return Err(Error::other("wallet scenario has insufficient funds"));
    }
    Ok(result)
}

fn asset(
    snapshot: &WalletSnapshot,
    kind: AssetKind,
    id: Option<Bytes32>,
) -> Result<AssetCoin, Error> {
    snapshot
        .assets
        .iter()
        .find(|a| a.kind == kind && id.is_none_or(|id| a.asset_id == id))
        .cloned()
        .ok_or_else(|| Error::other(format!("wallet did not discover {kind:?}")))
}

fn cat_balance(snapshot: &WalletSnapshot, id: Bytes32) -> u64 {
    snapshot
        .assets
        .iter()
        .filter(|a| a.kind == AssetKind::Cat2 && a.asset_id == id)
        .map(|a| a.record.coin.amount)
        .sum()
}

fn minted_asset(
    snapshot: &WalletSnapshot,
    bundle: &SpendBundle,
    kind: AssetKind,
) -> Result<AssetCoin, Error> {
    let parents: HashSet<_> = bundle.coin_spends.iter().map(|s| s.coin.name()).collect();
    snapshot
        .assets
        .iter()
        .find(|a| a.kind == kind && parents.contains(&a.record.coin.parent_coin_info))
        .cloned()
        .ok_or_else(|| Error::other("wallet did not discover the newly minted asset"))
}

async fn transfer_asset(
    sender: &FalconWallet,
    state: &WalletSnapshot,
    item: &AssetCoin,
    recipient: &FalconWallet,
    amount: u64,
    fee: u64,
) -> Result<SpendBundle, Error> {
    build_asset_transaction(
        sender,
        &funding(state, fee)?,
        &state.assets,
        &HashSet::new(),
        &AssetAction::Transfer {
            coin_id: item.record.coin.name(),
            destination: recipient.puzzle_hash(),
            amount,
        },
        fee,
        sender.puzzle_hash(),
        state.height + 1,
    )
    .await
}

/// This is intentionally stateful within one run: retries never re-sign pending spends.
pub async fn run(
    root: &Path,
    constants: &ConsensusConstants,
    clients: &[FullnodeClient],
    farmers: &[String],
) -> Result<Value, Error> {
    if clients.len() < 2
        || farmers.len() != 2
        || farmers[0] == farmers[1]
        || constants.signature_profile.is_bls()
    {
        return Err(Error::other(
            "wallet scenario requires two distinct Falcon farmers and at least two nodes",
        ));
    }
    let mut scenario = Scenario {
        clients,
        constants,
        transactions: Vec::new(),
    };
    let mut wallets = Vec::new();
    for farmer in farmers {
        let keys = FalconKeyring::load(&root.join(format!("farmer-{farmer}/falcon-keys.bin")))?;
        let payout = falcon_wallet::puzzle(&keys.wallet.public_key()).tree_hash();
        let wallet = FalconWallet::new(keys.wallet, Arc::new(*constants))?;
        let reward = loop {
            if let Ok(rewards) = clients[0]
                .get_coin_records_by_puzzle_hash(&payout, Some(false), None, None)
                .await
                && let Some(reward) = rewards
                    .into_iter()
                    .filter(|r| r.coinbase && r.coin.amount > 1_000_000)
                    .min_by_key(|r| (r.confirmed_block_index, r.coin.name().const_bytes()))
            {
                break reward;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        };
        let bundle = wallet.import_reward(reward.coin, 1, scenario.height().await? + 1)?;
        scenario
            .confirm(&format!("fund_wallet_{farmer}"), &bundle, 0)
            .await?;
        wallets.push(wallet);
    }
    let a = &wallets[0];
    let b = &wallets[1];
    for (label, sender, recipient, amount, node) in [
        ("native_a_to_b", a, b, 100_000, 0),
        ("native_b_to_a", b, a, 25_000, 1),
    ] {
        let state = sender.sync(&clients[node]).await?;
        let bundle = sender
            .payment(
                &funding(&state, amount + 1)?,
                &[Payment {
                    puzzle_hash: recipient.puzzle_hash(),
                    amount,
                }],
                1,
                state.height + 1,
            )
            .await?;
        scenario.reject_corrupt(&bundle).await?;
        scenario.confirm(label, &bundle, node).await?;
    }
    let state = a.sync(&clients[0]).await?;
    let mint = build_asset_transaction(
        a,
        &funding(&state, 1001)?,
        &[],
        &HashSet::new(),
        &AssetAction::LaunchCat2 { amount: 1000 },
        1,
        a.puzzle_hash(),
        state.height + 1,
    )
    .await?;
    scenario.confirm("cat_mint", &mint, 0).await?;
    let state = a.sync(&clients[0]).await?;
    let cat = minted_asset(&state, &mint, AssetKind::Cat2)?;
    let bundle = transfer_asset(a, &state, &cat, b, 400, 1).await?;
    scenario.reject_corrupt(&bundle).await?;
    scenario.confirm("cat_a_to_b", &bundle, 0).await?;
    let state = b.sync(&clients[1]).await?;
    if cat_balance(&state, cat.asset_id) != 400 {
        return Err(Error::other("CAT recipient balance mismatch"));
    }
    let received = asset(&state, AssetKind::Cat2, Some(cat.asset_id))?;
    let bundle = transfer_asset(b, &state, &received, a, 150, 1).await?;
    scenario.confirm("cat_b_to_a", &bundle, 1).await?;
    let state = a.sync(&clients[0]).await?;
    let mint = build_asset_transaction(
        a,
        &funding(&state, 2)?,
        &[],
        &HashSet::new(),
        &AssetAction::LaunchNft(NftLaunch {
            data_uri: "https://example.com/falcon-compose-nft".into(),
            data_hash: [42; 32].into(),
            royalty_basis_points: 250,
            royalty_puzzle_hash: a.puzzle_hash(),
            edition_number: 1,
            edition_total: 1,
        }),
        1,
        a.puzzle_hash(),
        state.height + 1,
    )
    .await?;
    scenario.confirm("nft_mint", &mint, 0).await?;
    let nft = minted_asset(&a.sync(&clients[0]).await?, &mint, AssetKind::Nft1)?;
    for (label, sender, recipient, node) in [("nft_a_to_b", a, b, 0), ("nft_b_to_a", b, a, 1)] {
        let state = sender.sync(&clients[node]).await?;
        let current = asset(&state, AssetKind::Nft1, Some(nft.asset_id))?;
        let bundle = transfer_asset(sender, &state, &current, recipient, 1, 1).await?;
        scenario.reject_corrupt(&bundle).await?;
        scenario.confirm(label, &bundle, node).await?;
        let received = asset(
            &recipient.sync(&clients[0]).await?,
            AssetKind::Nft1,
            Some(nft.asset_id),
        )?;
        if received.metadata != nft.metadata
            || received.royalty_basis_points != nft.royalty_basis_points
            || received.royalty_puzzle_hash != nft.royalty_puzzle_hash
        {
            return Err(Error::other(
                "NFT metadata or royalty changed during transfer",
            ));
        }
    }
    #[cfg(feature = "labrador")]
    if scenario.zk() {
        // Each owner proves locally. Their original Falcon witnesses are removed
        // before the combiner receives the two independent proofs.
        let mut independent = Vec::new();
        for (sender, recipient, node) in [(a, b, 0), (b, a, 1)] {
            let state = sender.sync(&clients[node]).await?;
            let payment = sender
                .payment(
                    &funding(&state, 101)?,
                    &[Payment {
                        puzzle_hash: recipient.puzzle_hash(),
                        amount: 100,
                    }],
                    1,
                    state.height + 1,
                )
                .await?;
            independent.push(scenario.finalize(&payment).await?);
        }
        let composed = combine(&independent, constants, scenario.height().await? + 1)?;
        scenario
            .confirm("independent_wallet_zk_proofs", &composed, 0)
            .await?;
    }
    // 254 native spends plus one CAT and one NFT spend, across both owners.
    // Distinct output amounts avoid duplicate coin IDs within each fan-out.
    let mut batches = Vec::new();
    for (index, sender, recipient) in [(0, a, b), (1, b, a)] {
        let outputs: Vec<_> = (0..127)
            .map(|i| Payment {
                puzzle_hash: sender.puzzle_hash(),
                amount: 1000 + i + index as u64 * 1000,
            })
            .collect();
        let needed = outputs.iter().map(|p| p.amount).sum::<u64>() + 1;
        let state = sender.sync(&clients[index]).await?;
        let split = sender
            .payment(&funding(&state, needed)?, &outputs, 1, state.height + 1)
            .await?;
        scenario
            .confirm(&format!("split_wallet_{index}"), &split, index)
            .await?;
        let state = sender.sync(&clients[index]).await?;
        let split_parents: HashSet<_> = split.coin_spends.iter().map(|s| s.coin.name()).collect();
        for output in &outputs {
            let record = state
                .native
                .iter()
                .find(|r| {
                    split_parents.contains(&r.coin.parent_coin_info)
                        && r.coin.amount == output.amount
                })
                .ok_or_else(|| Error::other("fan-out output not discovered by wallet"))?;
            batches.push(
                sender
                    .payment(
                        &[*record],
                        &[Payment {
                            puzzle_hash: recipient.puzzle_hash(),
                            amount: output.amount - 1,
                        }],
                        1,
                        state.height + 1,
                    )
                    .await?,
            );
        }
    }
    let state = b.sync(&clients[1]).await?;
    let remaining_cat = asset(&state, AssetKind::Cat2, Some(cat.asset_id))?;
    batches.push(
        transfer_asset(
            b,
            &state,
            &remaining_cat,
            a,
            remaining_cat.record.coin.amount,
            0,
        )
        .await?,
    );
    let state = a.sync(&clients[0]).await?;
    let current_nft = asset(&state, AssetKind::Nft1, Some(nft.asset_id))?;
    batches.push(transfer_asset(a, &state, &current_nft, b, 1, 0).await?);
    let combined = combine(&batches, constants, scenario.height().await? + 1)?;
    let combined = scenario.finalize(&combined).await?;
    let rpc_json_bytes = serde_json::to_vec(&json!({"spend_bundle": &combined}))
        .map_err(Error::other)?
        .len();
    let count = combined
        .authorization
        .as_ref()
        .ok_or_else(|| Error::other("missing batch authorization"))?
        .transactions
        .len();
    if count != 256 {
        return Err(Error::other(
            "mixed wallet batch must contain exactly 256 authorizations",
        ));
    }
    scenario.reject_corrupt(&combined).await?;
    let height = scenario
        .confirm("mixed_native_cat_nft_batch", &combined, 1)
        .await?;
    let mut proof_bytes = None;
    for client in clients {
        let record = client.get_block_record_by_height(height).await?;
        let block = client.get_block(&record.header_hash).await?;
        let auth = block
            .authorization
            .as_ref()
            .ok_or_else(|| Error::other("missing batch block authorization"))?;
        if constants.falcon_aggregation.is_some() {
            let proof = auth.labrador.as_ref().ok_or_else(|| {
                Error::other("mixed wallet batch was not aggregated with LaBRADOR")
            })?;
            if !auth.transactions.is_empty() || proof.len() >= count * 666 {
                return Err(Error::other(
                    "LaBRADOR did not reduce the mixed wallet batch",
                ));
            }
            if proof_bytes.is_some_and(|size| size != proof.len()) {
                return Err(Error::other("aggregate size differs across nodes"));
            }
            proof_bytes = Some(proof.len());
        } else if auth.transactions.len() != count || auth.labrador.is_some() {
            return Err(Error::other("direct Falcon batch authorization mismatch"));
        }
        let a_state = a.sync(client).await?;
        let b_state = b.sync(client).await?;
        if cat_balance(&a_state, cat.asset_id) != 1000 || cat_balance(&b_state, cat.asset_id) != 0 {
            return Err(Error::other("CAT supply or final owner balance mismatch"));
        }
        let final_nft = asset(&b_state, AssetKind::Nft1, Some(nft.asset_id))?;
        if a_state
            .assets
            .iter()
            .any(|a| a.kind == AssetKind::Nft1 && a.asset_id == nft.asset_id)
            || final_nft.metadata != nft.metadata
            || final_nft.royalty_basis_points != nft.royalty_basis_points
            || final_nft.royalty_puzzle_hash != nft.royalty_puzzle_hash
        {
            return Err(Error::other("NFT final ownership or metadata mismatch"));
        }
    }
    Ok(
        json!({"farmers":farmers,"wallets":[a.puzzle_hash(),b.puzzle_hash()],"transactions":scenario.transactions,
        "cat_asset_id":cat.asset_id,"cat_supply":1000,"nft_launcher_id":nft.asset_id,"nft_metadata_preserved":true,
        "nft_royalties_preserved":true,"corrupt_signatures_rejected":true,"wallet_zero_knowledge":scenario.zk(),
        "mixed_batch":{"height":height,"native_spends":254,"cat_spends":1,"nft_spends":1,"signatures":count,
            "direct_bytes":count*666,"labrador_bytes":proof_bytes,"rpc_json_bytes":rpc_json_bytes},"verified_nodes":clients.len()}),
    )
}
