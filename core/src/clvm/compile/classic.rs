use super::tokenizer::{Token, TokenType};
use super::{Compiler, NULL_SEXP, QUOTE_SEXP};
use crate::clvm::runtime::ClvmRuntime;
use crate::clvm::sexp::SExp;
use crate::constants::B_KEYWORD_TO_SEXP;
use std::borrow::Cow;
use std::io::{Error, ErrorKind};

#[derive(Debug, Clone)]
pub struct Macro {
    pub name: Vec<u8>,
    pub pattern: SExp<'static>,
    pub body: SExp<'static>,
}

pub fn expand<'a>(
    compiler: &'a Compiler<'a>,
    tokens: Vec<Token<'a>>,
) -> Result<Vec<Token<'a>>, Error> {
    let value = compiler.process_quoted(
        &mut tokens
            .into_iter()
            .filter(|token| token.t_type != TokenType::Comment)
            .collect::<Vec<_>>()
            .into_iter(),
        false,
    )?;
    let mut expansion = Expansion {
        macros: compiler.macros.read().clone(),
        remaining: 100_000,
    };
    let value = expansion.expand(&value, 0)?;
    let mut result = vec![];
    emit(&value, &mut result);
    Ok(result)
}

struct Expansion {
    macros: Vec<Macro>,
    remaining: usize,
}

impl Expansion {
    fn step(&mut self, depth: usize) -> Result<(), Error> {
        if depth >= 256 || self.remaining == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Classic macro expansion limit exceeded",
            ));
        }
        self.remaining -= 1;
        Ok(())
    }

    fn expand(&mut self, value: &SExp, depth: usize) -> Result<SExp<'static>, Error> {
        self.step(depth)?;
        let SExp::Pair(pair) = value else {
            return Ok(value.to_owned());
        };
        if let SExp::Atom(op) = pair.first() {
            if matches!(op.as_ref(), b"q" | b"quote") {
                return Ok(value.to_owned());
            }
            if let Some(definition) = self
                .macros
                .iter()
                .rev()
                .find(|m| m.name == op.as_ref())
                .cloned()
            {
                let mut bindings = vec![];
                bind(&definition.pattern, pair.rest(), &mut bindings, 0)?;
                let result = self.eval(&definition.body, &bindings, depth + 1)?;
                return self.expand(&result, depth + 1);
            }
        }
        let mut args = vec![];
        let mut tail = value;
        while let SExp::Pair(pair) = tail {
            args.push(self.expand(pair.first(), depth + 1)?);
            tail = pair.rest();
        }
        let mut result = tail.to_owned();
        while let Some(arg) = args.pop() {
            result = arg.cons(result);
        }
        Ok(result)
    }

    fn eval(
        &mut self,
        value: &SExp,
        bindings: &[(Vec<u8>, SExp<'static>)],
        depth: usize,
    ) -> Result<SExp<'static>, Error> {
        self.step(depth)?;
        let SExp::Pair(pair) = value else {
            let name = value.atom().map_err(Error::other)?;
            return Ok(bindings
                .iter()
                .rev()
                .find(|(key, _)| key == name.as_ref())
                .map_or_else(|| value.to_owned(), |(_, value)| value.clone()));
        };
        let op = pair.first().atom().map_err(Error::other)?;
        if op.as_ref() == b"q" {
            return Ok(pair.rest().to_owned());
        }
        let args = arguments(pair.rest())?;
        match op.as_ref() {
            b"quote" if args.len() == 1 => Ok(args[0].to_owned()),
            b"qq" if args.len() == 1 => self.quasiquote(&args[0], bindings, depth + 1),
            b"if" if args.len() == 3 => {
                let condition = self.eval(&args[0], bindings, depth + 1)?;
                self.eval(
                    &args[if condition == NULL_SEXP { 2 } else { 1 }],
                    bindings,
                    depth + 1,
                )
            }
            b"list" => {
                let mut values = vec![];
                for arg in args {
                    values.push(self.eval(&arg, bindings, depth + 1)?);
                }
                let mut result = NULL_SEXP;
                while let Some(value) = values.pop() {
                    result = value.cons(result);
                }
                Ok(result)
            }
            _ if self
                .macros
                .iter()
                .any(|definition| definition.name == op.as_ref()) =>
            {
                let expanded = self.expand(value, depth + 1)?;
                self.eval(&expanded, bindings, depth + 1)
            }
            _ => {
                let operator = B_KEYWORD_TO_SEXP.get(op.as_ref()).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Unsupported,
                        format!(
                            "Unsupported classic macro operator: {}",
                            String::from_utf8_lossy(op.as_ref())
                        ),
                    )
                })?;
                let mut values = vec![];
                for arg in args {
                    values.push(
                        QUOTE_SEXP
                            .clone()
                            .cons(self.eval(&arg, bindings, depth + 1)?),
                    );
                }
                let mut program = NULL_SEXP;
                while let Some(value) = values.pop() {
                    program = value.cons(program);
                }
                let program = operator.clone().cons(program);
                ClvmRuntime::new(10_000_000, 0)
                    .run(&program, &NULL_SEXP)
                    .map(|(_, value)| value)
                    .map_err(Error::other)
            }
        }
    }

    fn quasiquote(
        &mut self,
        value: &SExp,
        bindings: &[(Vec<u8>, SExp<'static>)],
        depth: usize,
    ) -> Result<SExp<'static>, Error> {
        self.step(depth)?;
        let SExp::Pair(pair) = value else {
            return Ok(value.to_owned());
        };
        if let SExp::Atom(op) = pair.first() {
            if op.as_ref() == b"qq" {
                let names = SExp::from(
                    bindings
                        .iter()
                        .map(|(name, _)| SExp::from(name.clone()))
                        .collect::<Vec<_>>(),
                );
                let body = SExp::from(vec![SExp::from(b"qq".to_vec()), value.to_owned()]);
                let module = SExp::from(vec![SExp::from(b"mod".to_vec()), names, body]);
                let mut tokens = vec![];
                emit(&module, &mut tokens);
                let source = tokens
                    .into_iter()
                    .flat_map(|token| {
                        let mut bytes = token.bytes.into_owned();
                        bytes.push(b' ');
                        bytes
                    })
                    .collect::<Vec<_>>();
                let compiler =
                    Compiler::new(Cow::Owned(source), crate::constants::COMPAT_CHIA, 0, &[]);
                let program = compiler.compile()?;
                let environment = crate::clvm::program::Program::new(SExp::from(
                    bindings
                        .iter()
                        .map(|(_, value)| value.clone())
                        .collect::<Vec<_>>(),
                ));
                return program
                    .run(10_000_000, 0, &environment)
                    .map(|(_, result)| result.sexp().to_owned())
                    .map_err(Error::other);
            }
            if op.as_ref() == b"unquote" {
                let args = arguments(pair.rest())?;
                if args.len() != 1 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected one unquote argument",
                    ));
                }
                return self.eval(&args[0], bindings, depth + 1);
            }
        }
        Ok(self
            .quasiquote(pair.first(), bindings, depth + 1)?
            .cons(self.quasiquote(pair.rest(), bindings, depth + 1)?))
    }
}

fn bind(
    pattern: &SExp,
    value: &SExp,
    bindings: &mut Vec<(Vec<u8>, SExp<'static>)>,
    depth: usize,
) -> Result<(), Error> {
    if depth >= 256 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Classic macro argument limit exceeded",
        ));
    }
    match pattern {
        SExp::Atom(atom) if atom.as_ref().is_empty() => {
            if *value != NULL_SEXP {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unexpected macro argument count",
                ));
            }
        }
        SExp::Atom(atom) => bindings.push((atom.as_ref().to_vec(), value.to_owned())),
        SExp::Pair(pair) => {
            let value = value.pair().map_err(Error::other)?;
            bind(pair.first(), value.first(), bindings, depth + 1)?;
            bind(pair.rest(), value.rest(), bindings, depth + 1)?;
        }
    }
    Ok(())
}

pub(super) fn emit(value: &SExp, tokens: &mut Vec<Token<'static>>) {
    let token = |bytes: Vec<u8>, t_type| Token {
        bytes: Cow::Owned(bytes),
        index: 0,
        t_type,
    };
    match value {
        SExp::Atom(atom) if !atom.as_ref().is_empty() => {
            let bytes = atom.as_ref();
            let symbol = bytes
                .iter()
                .all(|b| b.is_ascii_graphic() && !b"()\"';".contains(b))
                && !bytes.starts_with(b"#")
                && bytes != b"."
                && !bytes.starts_with(b"0x")
                && !bytes.starts_with(b"0X")
                && crate::clvm::assemble::handle_int(bytes).is_none();
            tokens.push(token(
                if symbol {
                    bytes.to_vec()
                } else {
                    format!("0x{}", hex::encode(bytes)).into_bytes()
                },
                TokenType::Expression,
            ));
        }
        _ => {
            tokens.push(token(b"(".to_vec(), TokenType::StartCons));
            let mut tail = value;
            while let SExp::Pair(pair) = tail {
                emit(pair.first(), tokens);
                tail = pair.rest();
            }
            if *tail != NULL_SEXP {
                tokens.push(token(b".".to_vec(), TokenType::DotCons));
                emit(tail, tokens);
            }
            tokens.push(token(b")".to_vec(), TokenType::EndCons));
        }
    }
}

fn arguments(value: &SExp) -> Result<Vec<SExp<'static>>, Error> {
    let mut args = vec![];
    let mut tail = value;
    while let SExp::Pair(pair) = tail {
        args.push(pair.first().to_owned());
        tail = pair.rest();
    }
    if *tail != NULL_SEXP {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Expected proper macro argument list",
        ));
    }
    Ok(args)
}
