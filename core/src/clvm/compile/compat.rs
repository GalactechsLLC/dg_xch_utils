use crate::clvm::compile::Compiler;
use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::utils::{eval_constant, parse_value, select_path};
use crate::clvm::sexp::SExp;
use crate::constants::{COMPAT_CHIA, CONS_SEXP, NULL_SEXP, QUOTE_SEXP};
use std::io::{Error, ErrorKind};
use std::sync::atomic::Ordering;
use std::vec::IntoIter;

impl<'a> Compiler<'a> {
    pub(super) fn process_quasiquote(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let token = token_stream
            .find(|token| token.t_type != TokenType::Comment)
            .ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected quasiquoted value",
            ))?;
        if token.t_type == TokenType::Expression {
            let value = parse_value(token.bytes.as_ref())?;
            return Ok(if value == NULL_SEXP {
                NULL_SEXP
            } else {
                QUOTE_SEXP.clone().cons(value)
            });
        }
        if token.t_type != TokenType::StartCons {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected quasiquoted value",
            ));
        }
        if let Some(token) = token_stream.as_slice().first() {
            if token.bytes.as_ref() == b"qq" {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Nested classic quasiquotation is not supported yet",
                ));
            }
            if token.bytes.as_ref() == b"unquote" {
                token_stream.next();
                let value =
                    self.process_expression(token_stream, function_args, mapped_args, env_depth)?;
                if token_stream
                    .next()
                    .is_none_or(|token| token.t_type != TokenType::EndCons)
                {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected one unquote argument",
                    ));
                }
                return Ok(value);
            }
        }
        let mut values = vec![];
        let mut tail = NULL_SEXP;
        loop {
            match token_stream.as_slice().first().map(|token| token.t_type) {
                Some(TokenType::Comment) => {
                    token_stream.next();
                }
                Some(TokenType::EndCons) => {
                    token_stream.next();
                    break;
                }
                Some(TokenType::DotCons) if !values.is_empty() => {
                    token_stream.next();
                    tail = self.process_quasiquote(
                        token_stream,
                        function_args,
                        mapped_args,
                        env_depth,
                    )?;
                    if token_stream
                        .next()
                        .is_none_or(|token| token.t_type != TokenType::EndCons)
                    {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "Expected closing quasiquote cons",
                        ));
                    }
                    break;
                }
                Some(_) => values.push(self.process_quasiquote(
                    token_stream,
                    function_args,
                    mapped_args,
                    env_depth,
                )?),
                None => {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "No closing quasiquote cons",
                    ));
                }
            }
        }
        while let Some(value) = values.pop() {
            tail = self.create_pair_sexp(vec![CONS_SEXP.clone(), value, tail])?;
        }
        Ok(tail)
    }

    pub(super) fn optimize_expression(&'a self, value: SExp<'a>) -> Result<SExp<'a>, Error> {
        let SExp::Pair(pair) = &value else {
            return Ok(value);
        };
        if pair.first() == &QUOTE_SEXP {
            return Ok(if pair.rest() == &NULL_SEXP {
                NULL_SEXP
            } else {
                value
            });
        }
        let mut entries = vec![pair.first().to_owned()];
        for arg in pair.rest().ref_list() {
            entries.push(self.optimize_expression(arg.to_owned())?);
        }
        if entries.len() == 2
            && matches!(&entries[0], SExp::Atom(atom) if matches!(atom.as_ref(), [5] | [6]))
        {
            return select_path(
                entries[1].clone(),
                num_bigint::BigInt::from(if entries[0] == SExp::from(5u8) {
                    2u8
                } else {
                    3u8
                }),
            );
        }
        if matches!(&entries[0], SExp::Atom(atom) if matches!(atom.as_ref(), [4] | [11])) {
            if let Some(result) = eval_constant(&entries) {
                return Ok(QUOTE_SEXP.clone().cons(result));
            }
        }
        self.create_pair_sexp(entries)
    }

    pub(super) fn select_path(
        &'a self,
        mut value: SExp<'a>,
        mut path: num_bigint::BigInt,
    ) -> Result<SExp<'a>, Error> {
        if self.compiler_version.load(Ordering::Relaxed) != 21 {
            return select_path(value, path);
        }
        while path > num_bigint::BigInt::from(1u8) {
            let right = (&path & num_bigint::BigInt::from(1u8)) == num_bigint::BigInt::from(1u8);
            value =
                self.create_pair_sexp(vec![SExp::from(if right { 6u8 } else { 5u8 }), value])?;
            path >>= 1;
        }
        Ok(value)
    }

    pub(super) fn get_path(&self, path: &num_bigint::BigInt) -> SExp<'static> {
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
            SExp::from(path.to_bytes_be().1)
        } else {
            SExp::from(path)
        }
    }

    pub(super) fn parse_sigil(&self, conditions_queue: &IntoIter<Token<'a>>) -> Result<(), Error> {
        let name = &conditions_queue.as_slice()[0];
        if self.flags & COMPAT_CHIA == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Chia compiler sigils require COMPAT_CHIA",
            ));
        }
        let version = match name.bytes.as_ref() {
            b"*standard-cl-21*" => 21,
            b"*standard-cl-23*" => 23,
            b"*standard-cl-25*" => 25,
            b"*standard-cl-26*" => 26,
            _ => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    format!(
                        "Unsupported compiler sigil: {}",
                        String::from_utf8_lossy(&name.bytes)
                    ),
                ));
            }
        };
        if conditions_queue.len() != 2
            || conditions_queue.as_slice()[1].t_type != TokenType::EndCons
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected end of include",
            ));
        }
        self.compiler_version.store(version, Ordering::Relaxed);
        Ok(())
    }
}
