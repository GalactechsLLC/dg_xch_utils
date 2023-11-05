#[test]
pub fn test_program() {
    use dg_xch_core::clvm::program::Program;
    use dg_xch_core::clvm::sexp::IntoSExp;
    use log::info;
    use simple_logger::SimpleLogger;
    SimpleLogger::new().env().init().unwrap();
    let program = Program::to(vec![
        10.to_sexp(),
        20.to_sexp(),
        30.to_sexp(),
        vec![15, 17].to_sexp(),
        40.to_sexp(),
        50.to_sexp(),
    ]);
    info!("{:?}", program);
    info!("{:?}", hex::encode(&program.serialized));
    assert_eq!(program.at("f").unwrap(), program.first().unwrap());
    assert_eq!(program.at("f").unwrap(), Program::to(10));
    assert_eq!(program.at("r").unwrap(), program.rest().unwrap());
    assert_eq!(
        program.at("r").unwrap(),
        Program::to(vec![
            20.to_sexp(),
            30.to_sexp(),
            vec![15, 17].to_sexp(),
            40.to_sexp(),
            50.to_sexp()
        ])
    );
    assert_eq!(
        program.at("rrrfrf").unwrap(),
        program
            .rest()
            .unwrap()
            .rest()
            .unwrap()
            .rest()
            .unwrap()
            .first()
            .unwrap()
            .rest()
            .unwrap()
            .first()
            .unwrap()
    );
    assert_eq!(program.at("rrrfrf").unwrap(), Program::to(17));
    assert!(program.at("q").is_err());
    assert!(program.at("ff").is_err());
}
