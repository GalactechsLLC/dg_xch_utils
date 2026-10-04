use dg_xch_core::blockchain::sized_bytes::Bytes32;
use dg_xch_core::clvm::{program::Program, sexp::SExp};
use dg_xch_core::utils::hash_256;

#[derive(Clone, Default)]
pub(crate) struct Conditions(Vec<SExp<'static>>);

impl Conditions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with(mut self, condition: SExp<'static>) -> Self {
        self.0.push(condition);
        self
    }
    pub fn extend(mut self, conditions: impl IntoIterator<Item = SExp<'static>>) -> Self {
        self.0.extend(conditions);
        self
    }
    pub fn create_coin(self, destination: Bytes32, amount: u64, hint: Option<Bytes32>) -> Self {
        let mut fields = vec![SExp::from(51), destination.into(), amount.into()];
        if let Some(hint) = hint {
            fields.push(SExp::from(vec![SExp::from(hint)]));
        }
        self.with(SExp::from(fields))
    }
    pub fn reserve_fee(self, fee: u64) -> Self {
        self.with(SExp::from(vec![SExp::from(52), fee.into()]))
    }
    pub fn create_coin_announcement(self, message: Vec<u8>) -> Self {
        self.with(SExp::from(vec![SExp::from(60), message.into()]))
    }
    pub fn assert_coin_announcement(self, id: Bytes32) -> Self {
        self.with(SExp::from(vec![SExp::from(61), id.into()]))
    }
    pub fn program(&self) -> Program<'static> {
        Program::to(self.0.clone())
    }
}

impl IntoIterator for Conditions {
    type Item = SExp<'static>;
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

pub(crate) fn announcement_id(coin: Bytes32, message: &[u8]) -> Bytes32 {
    let mut bytes = Vec::from(coin.as_ref() as &[u8]);
    bytes.extend_from_slice(message);
    hash_256(bytes).into()
}
