pub mod utils;

use std::collections::HashMap;
use std::io::Error;
use std::path::Path;
use std::rc::Rc;
use tokio::fs;
use crate::clvm::assemble::assemble_text;
use crate::clvm::compile::utils::newer;
use crate::clvm::parser::sexp_from_bytes;
use crate::clvm::sexp::{NULL, SExp};

fn include_dialect(
    dialects: &HashMap<Vec<u8>, i32>,
    e: &[SExp],
) -> Option<i32> {
    if let (SExp::Atom(inc), SExp::Atom(name)) = (&e[0], &e[1]) {
        if *inc.data == "include".as_bytes().to_vec() {
            if let Some(dialect) = dialects.get(allocator.buf(&name)) {
                return Some(*dialect);
            }
        }
    }

    None
}

pub fn detect_modern(sexp: &SExp) -> Option<i32> {
    let mut dialects = HashMap::new();
    dialects.insert("*standard-cl-21*".as_bytes().to_vec(), 21);
    dialects.insert("*standard-cl-22*".as_bytes().to_vec(), 22);
    sexp.proper_list(true).and_then(|l| {
        for elt in l.iter() {
            if let Some(dialect) = detect_modern(elt) {
                return Some(dialect);
            }
            match elt.proper_list(true) {
                None => {
                    continue;
                }
                Some(e) => {
                    if e.len() != 2 {
                        continue;
                    }
                    if let Some(dialect) = include_dialect(&dialects, &e) {
                        return Some(dialect);
                    }
                }
            }
        }
        None
    })
}

async fn  compile_clvm_text(
    search_paths: &[String],
    symbol_table: &mut HashMap<String, String>,
    text: &str,
    input_path: &str,
) -> Result<SExp, Error> {
    let assembled_sexp = sexp_from_bytes(assemble_text(text)?)?;
    if let Some(dialect) = detect_modern(&assembled_sexp) {
        let runner = Rc::new(DefaultProgramRunner::new());
        let opts = Rc::new(DefaultCompilerOpts::new(input_path))
            .set_optimize(true)
            .set_frontend_opt(dialect > 21)
            .set_search_paths(search_paths);
        let unopt_res = compile_file(runner.clone(), opts, text, symbol_table);
        let res = unopt_res.and_then(|x| run_optimizer(runner, Rc::new(x)));
        res.and_then(|x| {
            convert_to_clvm_rs(x).map_err(|r| match r {
                RunFailure::RunErr(l, x) => CompileErr(l, x),
                RunFailure::RunExn(l, x) => CompileErr(l, x.to_string()),
            })
        })
    } else {
        let compile_invoke_code = run();
        let input_sexp = allocator.new_pair(assembled_sexp, allocator.null())?;
        let run_program = run_program_for_search_paths(search_paths);
        let run_program_output =
            run_program.run_program(compile_invoke_code, input_sexp, None)?;
        Ok(run_program_output.1)
    }
}

pub async fn compile_clvm_inner(
    search_paths: &[String],
    symbol_table: &mut HashMap<String, String>,
    filename: &str,
    text: &str,
    result_stream: &mut Stream,
) -> Result<(), String> {
    let result = compile_clvm_text(search_paths, symbol_table, text, filename)
        .map_err(|x| format!("error {} compiling {}", x.1, disassemble(x.0)))?;
    sexp_to_stream(allocator, result, result_stream);
    Ok(())
}

pub async fn compile_clvm(
    input_path: &str,
    output_path: &str,
    search_paths: &[String],
    symbol_table: &mut HashMap<String, String>,
) -> Result<String, String> {
    let compile = newer(input_path, output_path).unwrap_or(true);
    let mut result_stream = vec![];
    if compile {
        let text = fs::read_to_string(input_path)
            .map_err(|x| format!("error reading {}: {:?}", input_path, x))?;
        compile_clvm_inner(
            search_paths,
            symbol_table,
            input_path,
            &text,
            &mut result_stream,
        )?;
        let output_path_obj = Path::new(output_path);
        let output_dir = output_path_obj
            .parent()
            .map(Ok)
            .unwrap_or_else(|| Err("could not get parent of output path"))?;
        let target_data = result_stream.get_value().hex();
        // Try to detect whether we'd put the same output in the output file.
        // Don't proceed if true.
        if let Ok(prev_content) = fs::read_to_string(output_path).await {
            let prev_trimmed = prev_content.trim();
            let trimmed = target_data.trim();
            if prev_trimmed == trimmed {
                // It's the same program, bail regardless.
                return Ok(output_path.to_string());
            }
        }
        // Make the contents appear atomically so that other test processes
        // won't mistake an empty file for intended output.
        let mut temp_output_file = NamedTempFile::new_in(output_dir).map_err(|e| {
            format!(
                "error creating temporary compiler output for {}: {:?}",
                input_path, e
            )
        })?;
        temp_output_file
            .write_all(target_data.as_bytes())
            .map_err(|_| format!("failed to write to {:?}", temp_output_file.path()))?;
        temp_output_file.persist(output_path).map_err(|e| {
            format!(
                "error persisting temporary compiler output {}: {:?}",
                output_path, e
            )
        })?;
    }
    Ok(output_path.to_string())
}