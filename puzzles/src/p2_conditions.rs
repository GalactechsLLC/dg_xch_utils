use dg_xch_core::clvm::program::Program;
use dg_xch_core::clvm::program::SerializedProgram;
use dg_xch_core::clvm::sexp::IntoSExp;
use dg_xch_core::clvm::utils::INFINITE_COST;
use lazy_static::lazy_static;
use std::io::Error;

const P2_CONDITIONS_HEX: &str = "ff04ffff0101ff0280";

lazy_static! {
    pub static ref MOD: Program = SerializedProgram::from_hex(P2_CONDITIONS_HEX)
        .unwrap()
        .to_program();
}

pub fn puzzle_for_conditions<T: IntoSExp>(conditions: T) -> Result<Program, Error> {
    let (_cost, result) = MOD.run(INFINITE_COST, 0, &Program::to(vec![conditions]))?;
    Ok(result)
}

pub fn solution_for_conditions<T: IntoSExp>(conditions: T) -> Result<Program, Error> {
    Ok(Program::to(vec![
        puzzle_for_conditions(conditions)?.to_sexp(),
        0.to_sexp(),
    ]))
}
