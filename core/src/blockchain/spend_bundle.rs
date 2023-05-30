use std::collections::HashSet;
use crate::blockchain::coin_spend::CoinSpend;
use crate::blockchain::sized_bytes::{Bytes32, Bytes96};
use crate::blockchain::sized_bytes::SizedBytes;
use crate::clvm::program::Program;
use blst::min_pk::{AggregateSignature, Signature};
use dg_xch_macros::ChiaSerial;
use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use crate::blockchain::coin::Coin;

#[derive(ChiaSerial, Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct SpendBundle {
    pub coin_spends: Vec<CoinSpend>,
    pub aggregated_signature: Bytes96,
}
impl SpendBundle {
    pub fn aggregate(bundles: Vec<SpendBundle>) -> Result<Self, Error> {
        let mut rtn = Self::empty();
        for bundle in bundles {
            rtn._aggregate(bundle)?;
        }
        Ok(rtn)
    }

    fn _aggregate(&mut self, other: Self) -> Result<(), Error> {
        let mut sigs: Vec<Signature> = vec![];
        if !self.aggregated_signature.is_null() {
            sigs.push((&self.aggregated_signature).try_into()?);
        }
        if !other.aggregated_signature.is_null() {
            sigs.push((&other.aggregated_signature).try_into()?);
        }
        self.coin_spends.extend(other.coin_spends);
        self.aggregated_signature = if sigs.is_empty() {
            Default::default()
        } else {
            AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?
                .to_signature()
                .into()
        };
        Ok(())
    }

    pub fn empty() -> Self {
        SpendBundle {
            coin_spends: vec![],
            aggregated_signature: Default::default(),
        }
    }

    pub fn output_conditions(&self) -> Result<Vec<Program>, Error> {
        let mut conditions = vec![];
        for spend in &self.coin_spends {
            let (_, output) = spend
                .puzzle_reveal
                .run_with_cost(u64::MAX, &spend.solution.to_program()?)?;
            conditions.extend(output.as_list())
        }
        Ok(conditions)
    }

    pub fn additions(&self) -> Result<Vec<Coin>, Error> {
        self.coin_spends.iter().try_fold(vec![], |mut prev, cur| {
            prev.extend(cur.additions()?);
            Ok(prev)
        })
    }

    pub fn removals(&self) -> Vec<Coin> {
        self.coin_spends.iter().map(|c| &c.coin).cloned().collect()
    }

    pub fn coins(&self) -> Vec<Coin> {
        self.removals()
    }

    pub fn net_additions(&self) -> Result<Vec<Coin>, Error> {
        let removals: HashSet<Bytes32> = HashSet::from_iter(self.removals().into_iter().map(|c| c.name()));
        Ok(self.additions()?.into_iter().filter(|a| {
            !removals.contains(&a.name())
        }).collect())
    }

    pub fn add_signature(mut self, sig: Signature) -> Result<Self, Error> {
        let mut sigs: Vec<Signature> = vec![sig];
        if !self.aggregated_signature.is_null() {
            sigs.push((&self.aggregated_signature).try_into()?);
        }
        self.aggregated_signature = if sigs.is_empty() {
            Default::default()
        } else {
            AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
                .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?
                .to_signature()
                .into()
        };
        Ok(self)
    }

    pub fn sign<T: Fn(&CoinSpend) -> Result<Signature, Error>>(mut self, sig_function: T) -> Result<Self, Error> {
        let mut sigs: Vec<Signature> = vec![];
        for spend in &self.coin_spends {
            sigs.push( (sig_function)(spend)?);
        }
        self.aggregated_signature = AggregateSignature::aggregate(&sigs.iter().collect::<Vec<&Signature>>(), true)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, format!("{:?}", e)))?
            .to_signature()
            .into();
        Ok(self)
    }
}