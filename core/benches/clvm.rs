use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use dg_parser_macro::parse_program_hex;
use dg_xch_core::clvm::program::{Program, SerializedProgram};
use dg_xch_core::clvm::runtime::ClvmRuntime;
use dg_xch_core::clvm::sexp::SExp;
use dg_xch_core::clvm::utils::{MEMPOOL_MODE, NO_UNKNOWN_OPS};
use dg_xch_core::consensus::block_generator::{
    BlockGeneratorFlags, BlockGeneratorInput, GeneratorReference, execute_block_generator_result,
};
use dg_xch_core::consensus::constants::MAINNET;
use std::hint::black_box;

const BLOCK_834752: &str = include_str!("../tests/fixtures/chia_generator_tests/block-834752.txt");
const BLOCK_4671894: &str =
    include_str!("../tests/fixtures/chia_generator_tests/block-4671894.txt");
const BLOCK_4671894_REF: &str =
    include_str!("../tests/fixtures/chia_generator_tests/block-4671894.env");
const BLOCK_6755796: &str =
    include_str!("../tests/fixtures/chia_generator_tests/block-6755796.txt");

parse_program_hex!(
    SIMPLE_MATH_TEST,
    "ff02ffff01ff02ff06ffff04ff02ffff04ff03ffff04ff8205bbffff04ff8202bbffff04ff82013bffff04ff8200bbffff04ff5bffff04ff2bffff04ff13ff8080808080808080808080ffff04ffff01ffff12ffff10ff05ff0b80ff0b80ff22ffff02ff04ffff04ff02ffff04ff8202ffffff04ff82017fff8080808080ffff02ff04ffff04ff02ffff04ff2fffff04ff17ff8080808080ffff0bff8200bf80ffff0bff0b8080ff018080"
);

fn bench_simple_math(c: &mut Criterion) {
    let mut g = c.benchmark_group("bench_simple_math");
    g.bench_function("Curried Simple Math".to_string(), |b| {
        let mut runtime = ClvmRuntime::new(u64::MAX, MEMPOOL_MODE | NO_UNKNOWN_OPS);
        let args = Program::to(&[SExp::from(&[
            SExp::from(2),
            SExp::from(3),
            SExp::from(b"Some Binary Data".to_vec()),
            SExp::from(&[
                SExp::from(4),
                SExp::from(5),
                SExp::from(b"Some Binary Data".to_vec()),
            ]),
        ])]);
        let args_hash = args.tree_hash();
        let args_program = vec![Program::to(args_hash)];
        let program = SIMPLE_MATH_TEST_PROGRAM.curry(&args_program);
        b.iter(|| {
            let (cost, out) = runtime.run(program.sexp(), args.sexp()).unwrap();
            black_box(cost);
            assert_eq!(out, SExp::from(1));
            black_box(out);
        })
    });
    g.finish();
}

fn generator_input(
    fixture: &str,
    height: u32,
    generator_refs: Vec<GeneratorReference>,
) -> BlockGeneratorInput {
    let generator = SerializedProgram::from_hex(fixture.lines().next().unwrap()).unwrap();
    BlockGeneratorInput {
        transactions_generator: generator,
        generator_refs,
        constants: MAINNET,
        height,
        flags: BlockGeneratorFlags::for_height(&MAINNET, height),
    }
}

// Runs execute_block_generator_result over real mainnet transaction blocks and
// reports CLVM cost/sec (Throughput::Elements = cost). block-4671894 is a large
// generator (532 spends, ~4.05e9 cost) carrying a back-reference block.
fn bench_block_generator(c: &mut Criterion) {
    let cases = [
        (
            "block-834752",
            generator_input(BLOCK_834752, 834_752, vec![]),
        ),
        (
            "block-6755796",
            generator_input(BLOCK_6755796, 6_755_796, vec![]),
        ),
        (
            "block-4671894",
            generator_input(
                BLOCK_4671894,
                4_671_894,
                vec![GeneratorReference {
                    height: 4_671_893,
                    index: 0,
                    generator: SerializedProgram::from_hex(BLOCK_4671894_REF).unwrap(),
                }],
            ),
        ),
    ];
    let mut g = c.benchmark_group("block_generator_execute");
    for (name, input) in &cases {
        let cost = execute_block_generator_result(input).unwrap().cost;
        g.throughput(Throughput::Elements(cost));
        g.bench_function(*name, |b| {
            b.iter(|| {
                let conds = execute_block_generator_result(black_box(input)).unwrap();
                black_box(conds.cost);
            });
        });
    }
    g.finish();
}

criterion_group!(benches, bench_simple_math, bench_block_generator);
criterion_main!(benches);
