use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::{COMPAT_CHIA, Compiler, Constant, QUOTE_SEXP};
use crate::clvm::runtime::ClvmRuntime;
use crate::clvm::sexp::SExp;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{Error, ErrorKind};
use std::sync::atomic::Ordering;
use std::vec::IntoIter;
const DEFCONST_EVAL_COST: u64 = 1_000_000_000;

enum Placement {
    Unused,
    Inline,
    Tabled,
}

impl<'a> Compiler<'a> {
    pub(super) fn process_com(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        self.com_depth.fetch_add(1, Ordering::Relaxed);
        let code = self.process_expression(token_stream, function_args, mapped_args, env_depth);
        self.com_depth.fetch_sub(1, Ordering::Relaxed);
        let code = code?;
        if token_stream
            .find(|token| token.t_type != TokenType::Comment)
            .is_none_or(|token| token.t_type != TokenType::EndCons)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected exactly one argument for com",
            ));
        }
        Ok(QUOTE_SEXP.clone().cons(code))
    }
    pub(super) fn evaluate_defconsts(&'a self) -> Result<(), Error> {
        let defconsts = std::mem::take(&mut *self.defconsts.write());
        if defconsts.is_empty() {
            return Ok(());
        }
        if self.flags & COMPAT_CHIA == 0 || self.compiler_version.load(Ordering::Relaxed) < 21 {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Expected a Chia compiler sigil for defconst",
            ));
        }
        let names: HashSet<Cow<'a, [u8]>> =
            defconsts.iter().map(|c| c.name.bytes.clone()).collect();
        let dependencies: Vec<HashSet<Cow<'a, [u8]>>> = defconsts
            .iter()
            .map(|constant| {
                self.reachable_names(&constant.value)
                    .into_iter()
                    .filter(|name| names.contains(name) && *name != constant.name.bytes)
                    .collect()
            })
            .collect();
        let mut values: Vec<(Token<'a>, SExp<'static>)> = vec![];
        let mut pending: Vec<(&Constant<'a>, &HashSet<Cow<'a, [u8]>>)> =
            defconsts.iter().zip(dependencies.iter()).collect();
        // Evaluate in dependency order
        while !pending.is_empty() {
            let ready = pending.iter().position(|(_, needs)| {
                needs
                    .iter()
                    .all(|need| values.iter().any(|(name, _)| &name.bytes == need))
            });
            let Some(index) = ready else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Dependency cycle in defconst: {}",
                        pending
                            .iter()
                            .map(|(c, _)| String::from_utf8_lossy(&c.name.bytes).into_owned())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            };
            let (constant, _) = pending.remove(index);
            let value = self.evaluate_defconst(constant, &values)?;
            values.push((constant.name.clone(), value));
        }
        let tabled_dialect = self.compiler_version.load(Ordering::Relaxed) > 22;
        for (name, value) in values {
            let placement = if tabled_dialect {
                self.placement(&name)
            } else {
                Placement::Inline
            };
            match placement {
                Placement::Unused | Placement::Inline => {
                    self.declaration_order
                        .write()
                        .retain(|declared| declared != &name.bytes);
                }
                Placement::Tabled => {}
            }
            if !matches!(placement, Placement::Unused) {
                self.embedded_files.write().push((name, value));
            }
        }
        Ok(())
    }
    fn evaluate_defconst(
        &'a self,
        constant: &Constant<'a>,
        evaluated: &[(Token<'a>, SExp<'static>)],
    ) -> Result<SExp<'static>, Error> {
        let mut source = format!(
            "(mod () (include *standard-cl-{}*) ",
            self.compiler_version.load(Ordering::Relaxed)
        );
        for declaration in self.declarations.read().iter() {
            write_tokens(&mut source, declaration);
            source.push(' ');
        }
        for (name, value) in evaluated {
            source.push_str("(defconstant ");
            source.push_str(&String::from_utf8_lossy(&name.bytes));
            source.push(' ');
            write_value(&mut source, value);
            source.push_str(") ");
        }
        write_tokens(&mut source, &constant.value);
        source.push(')');
        let mut compiler = Compiler::new(
            Cow::Owned(source.into_bytes()),
            self.flags,
            self.opt_level,
            self.include_dirs,
        );
        compiler.compile_depth = self.compile_depth + 1;
        let constant_name = String::from_utf8_lossy(&constant.name.bytes).into_owned();
        let program = compiler.compile().map_err(|e| {
            Error::new(
                e.kind(),
                format!("Failed to compile defconst {constant_name}: {e}"),
            )
        })?;
        ClvmRuntime::new(DEFCONST_EVAL_COST, 0)
            .run(program.sexp(), &crate::constants::NULL_SEXP)
            .map(|(_, value)| value)
            .map_err(|e| {
                Error::new(
                    ErrorKind::InvalidInput,
                    format!("Failed to evaluate defconst {constant_name}: {e}"),
                )
            })
    }
    fn reachable_names(&self, tokens: &[Token<'a>]) -> HashSet<Cow<'a, [u8]>> {
        let bodies: HashMap<Cow<'a, [u8]>, Vec<Token<'a>>> = self
            .functions
            .read()
            .iter()
            .chain(self.inline_functions.read().iter())
            .map(|function| (function.name.bytes.clone(), function.function_body.clone()))
            .collect();
        let mut seen: HashSet<Cow<'a, [u8]>> = HashSet::new();
        let mut pending: Vec<Cow<'a, [u8]>> = tokens.iter().map(|t| t.bytes.clone()).collect();
        while let Some(name) = pending.pop() {
            if seen.insert(name.clone())
                && let Some(body) = bodies.get(&name)
            {
                pending.extend(body.iter().map(|t| t.bytes.clone()));
            }
        }
        seen
    }

    fn placement(&'a self, name: &Token<'a>) -> Placement {
        let inline: HashMap<Cow<'a, [u8]>, Vec<Token<'a>>> = self
            .inline_functions
            .read()
            .iter()
            .map(|function| (function.name.bytes.clone(), function.function_body.clone()))
            .collect();
        let in_functions = self
            .functions
            .read()
            .iter()
            .any(|function| function.function_body.iter().any(|t| t.bytes == name.bytes));
        if in_functions {
            return Placement::Tabled;
        }
        match count_uses(&self.body.lock(), &name.bytes, &inline, &mut vec![]) {
            None => Placement::Tabled,
            Some(0) => Placement::Unused,
            Some(1) => Placement::Inline,
            Some(_) => Placement::Tabled,
        }
    }
}
fn count_uses<'a>(
    tokens: &[Token<'a>],
    name: &[u8],
    inline: &HashMap<Cow<'a, [u8]>, Vec<Token<'a>>>,
    expanding: &mut Vec<Cow<'a, [u8]>>,
) -> Option<usize> {
    let mut count = 0;
    let mut com_depths: Vec<usize> = vec![];
    let mut depth = 0usize;
    let mut operator_position = false;
    for token in tokens {
        match token.t_type {
            TokenType::StartCons => {
                depth += 1;
                operator_position = true;
                continue;
            }
            TokenType::EndCons => {
                if com_depths.last() == Some(&depth) {
                    com_depths.pop();
                }
                depth = depth.saturating_sub(1);
            }
            TokenType::Expression => {
                if operator_position && token.bytes.as_ref() == b"com" {
                    com_depths.push(depth);
                } else if token.bytes.as_ref() == name {
                    if !com_depths.is_empty() {
                        return None;
                    }
                    count += 1;
                } else if operator_position
                    && let Some(body) = inline.get(&token.bytes)
                    && !expanding.contains(&token.bytes)
                {
                    expanding.push(token.bytes.clone());
                    let nested = count_uses(body, name, inline, expanding);
                    expanding.pop();
                    match nested {
                        None => return None,
                        Some(n) if n > 0 && !com_depths.is_empty() => return None,
                        Some(n) => count += n,
                    }
                }
            }
            TokenType::DotCons | TokenType::Comment => {}
        }
        operator_position = false;
    }
    Some(count)
}

fn write_tokens(out: &mut String, tokens: &[Token<'_>]) {
    for token in tokens {
        if token.t_type == TokenType::Comment {
            continue;
        }
        out.push_str(&String::from_utf8_lossy(&token.bytes));
        out.push(' ');
    }
}
fn write_value(out: &mut String, value: &SExp<'_>) {
    match value {
        SExp::Atom(atom) => {
            if atom.as_ref().is_empty() {
                out.push_str("()");
            } else {
                out.push_str("0x");
                out.push_str(&hex::encode(atom.as_ref()));
            }
        }
        SExp::Pair(pair) => {
            out.push('(');
            write_value(out, pair.first());
            out.push_str(" . ");
            write_value(out, pair.rest());
            out.push(')');
        }
    }
}
