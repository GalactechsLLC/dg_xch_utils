//! Shared exact streaming aggregation and bounded top-page collection.
use crate::StoreError;
use dg_xch_core::{blockchain::coin_record::CoinRecord, protocols::explorer::CoinSummary};

#[derive(Default)]
pub(crate) struct Summary {
    balance: u128,
    received: u128,
    spent: u128,
    count: u64,
    unspent: u64,
    first: Option<u64>,
    last: Option<u64>,
    activity: Option<u32>,
}
impl Summary {
    pub fn add(&mut self, record: &CoinRecord, height: u32) -> Result<(), StoreError> {
        if record.confirmed_block_index > height {
            return Ok(());
        }
        let spent = record.spent_block_index != 0 && record.spent_block_index <= height;
        let amount = u128::from(record.coin.amount);
        self.received = self
            .received
            .checked_add(amount)
            .ok_or_else(|| StoreError::Corrupt("Amount aggregate overflow".into()))?;
        if spent {
            self.spent += amount;
        } else {
            self.balance += amount;
            self.unspent += 1;
        }
        self.count += 1;
        self.first = Some(
            self.first
                .map_or(record.timestamp, |v| v.min(record.timestamp)),
        );
        self.last = Some(
            self.last
                .map_or(record.timestamp, |v| v.max(record.timestamp)),
        );
        let activity =
            record
                .confirmed_block_index
                .max(if spent { record.spent_block_index } else { 0 });
        self.activity = Some(self.activity.map_or(activity, |v| v.max(activity)));
        Ok(())
    }
    pub fn finish(self) -> CoinSummary {
        CoinSummary {
            balance: self.balance.to_string(),
            received: self.received.to_string(),
            spent: self.spent.to_string(),
            coin_count: self.count,
            unspent_count: self.unspent,
            first_seen: self.first,
            last_received: self.last,
            last_activity_height: self.activity,
        }
    }
}
pub(crate) fn work_deadline(start: std::time::Instant) -> Result<(), StoreError> {
    if start.elapsed() > std::time::Duration::from_secs(5) {
        return Err(StoreError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Explorer query work deadline exceeded; no partial result returned",
        )));
    }
    Ok(())
}
