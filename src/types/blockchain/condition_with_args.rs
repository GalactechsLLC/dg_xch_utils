use crate::types::blockchain::condition_opcode::ConditionOpcode;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ConditionWithArgs {
    pub opcode: ConditionOpcode,
    pub vars: Vec<Vec<u8>>,
}
