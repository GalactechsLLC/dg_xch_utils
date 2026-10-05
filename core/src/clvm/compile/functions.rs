use crate::clvm::compile::conditions::parse_assign_pattern;
use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::utils::{get_arg_pointer, get_function_pointer};
use crate::clvm::compile::{Compiler, Function};
use crate::clvm::sexp::SExp;
use crate::constants::{APPLY_SEXP, CONS_SEXP, NULL_SEXP, QUOTE_SEXP};
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Error, ErrorKind};
use std::sync::atomic::Ordering;

impl<'a> Compiler<'a> {
    pub(super) fn get_function(
        &'a self,
        token: Token<'a>,
        mut args: Vec<SExp<'a>>,
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let (index, num_args, has_rest_arg) = self
            .functions
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.name.bytes == token.bytes)
            .map(|v| {
                (
                    v.0,
                    v.1.argument_names.len() - usize::from(v.1.has_rest_arg),
                    v.1.has_rest_arg,
                )
            })
            .ok_or(Error::new(ErrorKind::InvalidData, "Function not found"))?;
        if self.compiler_version.load(Ordering::Relaxed) >= 23
            && (args.len() < num_args || (!has_rest_arg && args.len() != num_args))
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Unexpected Function argument count for {}: got {}, expected {}",
                    String::from_utf8_lossy(&token.bytes),
                    args.len(),
                    num_args
                ),
            ));
        }
        let (index, count) = if self.table_names.read().is_empty() {
            (index, self.functions.read().len())
        } else {
            (
                self.table_names
                    .read()
                    .iter()
                    .position(|name| name == &token.bytes)
                    .unwrap(),
                self.table_names.read().len(),
            )
        };
        let func_pointer = get_function_pointer(index, 0, count, true)?;
        let mut result = NULL_SEXP.clone();
        while let Some(arg) = args.pop() {
            result = self.create_pair_sexp(vec![CONS_SEXP.clone(), arg, result])?;
        }
        let env = self.create_pair_sexp(vec![
            CONS_SEXP.clone(),
            self.get_path(&(num_bigint::BigInt::from(2u8) << env_depth)),
            result,
        ])?;
        self.create_pair_sexp(vec![
            APPLY_SEXP.clone(),
            self.get_path(&(func_pointer << env_depth)),
            env,
        ])
    }

    pub(super) fn get_program_args_sexp(&'a self) -> Result<SExp<'a>, Error> {
        let mut entries = vec![];
        if self.table_names.read().is_empty() {
            for func in self.functions.read().clone().into_iter() {
                entries.push(self.get_function_body(func)?);
            }
        } else {
            for name in self.table_names.read().clone() {
                if let Some(function) = self
                    .functions
                    .read()
                    .iter()
                    .find(|f| f.name.bytes == name)
                    .cloned()
                {
                    entries.push(self.get_function_body(function)?);
                } else if let Some(constant) =
                    self.constants.read().iter().find(|v| v.name.bytes == name)
                {
                    entries
                        .push(self.process_quoted(&mut constant.value.clone().into_iter(), true)?);
                } else {
                    entries.push(
                        self.embedded_files
                            .read()
                            .iter()
                            .find(|(token, _)| token.bytes == name)
                            .unwrap()
                            .1
                            .clone(),
                    );
                }
            }
        }
        if entries.is_empty() {
            return self.create_pair_sexp(vec![CONS_SEXP.clone(), NULL_SEXP, SExp::from(1u8)]);
        }
        entries = vec![Self::get_function_tree(&entries)];
        entries.push(QUOTE_SEXP.clone());
        let mut rtn = None;
        for arg in entries.into_iter() {
            match rtn {
                None => rtn = Some(arg),
                Some(r) => {
                    rtn = Some(arg.cons(r));
                }
            }
        }
        self.create_pair_sexp(vec![
            CONS_SEXP.clone(),
            rtn.unwrap_or(NULL_SEXP.clone()),
            SExp::from(1u8),
        ])
    }

    fn get_function_tree(entries: &[SExp<'a>]) -> SExp<'a> {
        if entries.len() == 1 {
            entries[0].clone()
        } else {
            let middle = entries.len() / 2;
            Self::get_function_tree(&entries[..middle])
                .cons(Self::get_function_tree(&entries[middle..]))
        }
    }

    pub(super) fn get_function_body(&'a self, function: Function<'a>) -> Result<SExp<'a>, Error> {
        let mut args = vec![];
        for index in 0..function.argument_names.len() {
            if function.has_rest_arg && index + 1 == function.argument_names.len() {
                // The rest argument selects the tail, rather than its first element.
                let path = (num_bigint::BigInt::from(1u8) << (index + 2)) - 1u8;
                args.push(self.get_path(&path));
            } else {
                args.push(self.get_path(&get_arg_pointer(index + 1)?));
            }
        }
        let lambda_pattern = if function.name.bytes.starts_with(b"\0lambda_") {
            Some(function.argument_pattern.clone())
        } else {
            None
        };
        let mut names = function.argument_names;
        if !function.argument_pattern.is_empty() {
            let mut bindings = vec![];
            parse_assign_pattern(
                &mut function.argument_pattern.into_iter(),
                num_bigint::BigInt::from(3u8),
                &mut bindings,
            )?;
            names = bindings
                .iter()
                .rev()
                .map(|(name, _)| name.clone())
                .collect();
            args = bindings
                .iter()
                .rev()
                .map(|(_, path)| self.get_path(path))
                .collect();
        }
        // A lambda reconstructs its captured arguments before entering generated helpers.
        let env = if let Some(pattern) = lambda_pattern {
            let pattern = self.process_quoted(&mut pattern.into_iter(), false)?;
            let scope = self.rebuild_arguments(&pattern, &names, &args)?;
            self.create_pair_sexp(vec![CONS_SEXP.clone(), SExp::from(2u8), scope])?
        } else {
            SExp::from(1u8)
        };
        names.push(Token {
            bytes: Cow::Borrowed(b"@*env*"),
            index: 0,
            t_type: TokenType::Expression,
        });
        args.push(env);
        let value =
            self.process_expression(&mut function.function_body.into_iter(), &names, &args, 0)?;
        if self.compiler_version.load(Ordering::Relaxed) == 21 {
            // CL21 optimizes each compiled function, but leaves quoted branches intact.
            self.optimize_expression(value)
        } else {
            Ok(value)
        }
    }

    fn rebuild_arguments(
        &'a self,
        pattern: &SExp,
        names: &[Token<'a>],
        args: &[SExp<'a>],
    ) -> Result<SExp<'a>, Error> {
        match pattern {
            SExp::Atom(atom) if atom.as_ref().is_empty() => Ok(NULL_SEXP),
            SExp::Atom(atom) => names
                .iter()
                .position(|name| name.bytes.as_ref() == atom.as_ref())
                .map(|index| args[index].clone())
                .ok_or(Error::new(ErrorKind::InvalidInput, "Unknown capture name")),
            SExp::Pair(pair) => self.create_pair_sexp(vec![
                CONS_SEXP.clone(),
                self.rebuild_arguments(pair.first(), names, args)?,
                self.rebuild_arguments(pair.rest(), names, args)?,
            ]),
        }
    }

    pub(super) fn curry_value(
        &'a self,
        program: SExp<'a>,
        argument: SExp<'a>,
    ) -> Result<SExp<'a>, Error> {
        let list = |values: Vec<SExp<'a>>| -> Result<SExp<'a>, Error> {
            let mut result = NULL_SEXP;
            for value in values.into_iter().rev() {
                result = self.create_pair_sexp(vec![CONS_SEXP.clone(), value, result])?;
            }
            Ok(result)
        };
        let quote_program = self.create_pair_sexp(vec![
            CONS_SEXP.clone(),
            QUOTE_SEXP.clone().cons(QUOTE_SEXP.clone()),
            program,
        ])?;
        let quote_argument = self.create_pair_sexp(vec![
            CONS_SEXP.clone(),
            QUOTE_SEXP.clone().cons(QUOTE_SEXP.clone()),
            argument,
        ])?;
        let env = list(vec![
            QUOTE_SEXP.clone().cons(CONS_SEXP.clone()),
            quote_argument,
            QUOTE_SEXP.clone().cons(SExp::from(1u8)),
        ])?;
        list(vec![
            QUOTE_SEXP.clone().cons(APPLY_SEXP.clone()),
            quote_program,
            env,
        ])
    }

    pub(super) fn get_inline_function(
        &'a self,
        token: Token<'a>,
        mut args: Vec<SExp<'a>>,
        caller_names: &[Token<'a>],
        caller_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let func = self
            .inline_functions
            .read()
            .iter()
            .find(|v| v.name.bytes == token.bytes)
            .cloned()
            .ok_or(Error::new(ErrorKind::InvalidData, "Inline Func not found"))?;
        let num_args = func.argument_names.len() - usize::from(func.has_rest_arg);
        if args.len() < num_args || (!func.has_rest_arg && args.len() != num_args) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!(
                    "Unexpected Function argument count for {}: got {}, expected {}",
                    String::from_utf8_lossy(&token.bytes),
                    args.len(),
                    num_args
                ),
            ));
        }
        let mut scope = NULL_SEXP;
        for arg in args.iter().rev() {
            scope = self.create_pair_sexp(vec![
                self.byte_atom(vec![crate::constants::CONS]),
                arg.clone(),
                scope,
            ])?;
        }
        let env = self.create_pair_sexp(vec![
            self.byte_atom(vec![crate::constants::CONS]),
            if self.functions.read().is_empty() && self.table_names.read().is_empty() {
                NULL_SEXP
            } else {
                self.get_path(&(num_bigint::BigInt::from(2u8) << env_depth))
            },
            scope.clone(),
        ])?;
        let mut names = func.argument_names.clone();
        if func.argument_pattern.len() > 1 {
            let mut bindings = vec![];
            parse_assign_pattern(
                &mut func.argument_pattern.clone().into_iter(),
                num_bigint::BigInt::from(1u8),
                &mut bindings,
            )?;
            names = bindings
                .iter()
                .rev()
                .map(|(name, _)| name.clone())
                .collect();
            args = bindings
                .iter()
                .rev()
                .map(|(_, path)| {
                    let mut path = path.clone();
                    let mut index = 0;
                    while (&path & num_bigint::BigInt::from(1u8)) == num_bigint::BigInt::from(1u8)
                        && path > num_bigint::BigInt::from(1u8)
                    {
                        index += 1;
                        path >>= 1;
                    }
                    if path == num_bigint::BigInt::from(1u8) {
                        let mut rest = args[index..].to_vec();
                        let program = if self.compiler_version.load(Ordering::Relaxed) == 0
                            && !rest.is_empty()
                        {
                            Some(rest.remove(0))
                        } else {
                            None
                        };
                        let mut result = if self.compiler_version.load(Ordering::Relaxed) == 21 {
                            QUOTE_SEXP.clone().cons(NULL_SEXP)
                        } else {
                            NULL_SEXP
                        };
                        while let Some(arg) = rest.pop() {
                            result = self.create_pair_sexp(vec![CONS_SEXP.clone(), arg, result])?;
                        }
                        return if let Some(program) = program {
                            let env = self.create_pair_sexp(vec![
                                CONS_SEXP.clone(),
                                self.get_path(&(num_bigint::BigInt::from(2u8) << env_depth)),
                                result,
                            ])?;
                            self.create_pair_sexp(vec![APPLY_SEXP.clone(), program, env])
                        } else {
                            Ok(result)
                        };
                    }
                    let arg = args.get(index).ok_or(Error::new(
                        ErrorKind::InvalidInput,
                        "Missing inline argument",
                    ))?;
                    self.select_path(arg.clone(), path >> 1usize)
                })
                .collect::<Result<Vec<_>, _>>()?;
        } else if func.has_rest_arg {
            let mut rest_args = args.split_off(num_args);
            let mut result = NULL_SEXP.clone();
            while let Some(arg) = rest_args.pop() {
                result = self.create_pair_sexp(vec![CONS_SEXP.clone(), arg, result])?;
            }
            args.push(result);
        }
        let capture_scope = self.compiler_version.load(Ordering::Relaxed) == 0
            && self.inline_stack.lock().is_empty();
        if capture_scope {
            *self.inline_scope.lock() = caller_names
                .iter()
                .zip(caller_args)
                .map(|(name, value)| {
                    (
                        Token {
                            bytes: Cow::Owned(name.bytes.to_vec()),
                            index: name.index,
                            t_type: name.t_type,
                        },
                        value.to_owned(),
                    )
                })
                .collect();
        }
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
            // Free names resolve in the surrounding function, not another inline's parameters.
            for (name, value) in self.inline_scope.lock().iter() {
                if name.bytes.as_ref() != b"@*env*"
                    && !names.iter().any(|arg| arg.bytes == name.bytes)
                {
                    names.push(name.clone());
                    args.push(value.clone());
                }
            }
        }
        {
            let mut inline_stack = self.inline_stack.lock();
            if inline_stack.contains(&token.bytes) {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Recursive inline function: {}",
                        String::from_utf8_lossy(&token.bytes)
                    ),
                ));
            }
            inline_stack.push(token.bytes);
        }
        names.push(Token {
            bytes: Cow::Borrowed(b"@*env*"),
            index: 0,
            t_type: TokenType::Expression,
        });
        args.push(env);
        let result = self.process_expression(
            &mut func.function_body.into_iter(),
            &names,
            &args,
            env_depth,
        );
        self.inline_stack.lock().pop();
        if capture_scope {
            self.inline_scope.lock().clear();
        }
        if self.compiler_version.load(Ordering::Relaxed) == 23
            && func.has_rest_arg
            && func.argument_pattern.len() > 1
        {
            // Destructuring a rest list can expose constant tails after substitution.
            result.and_then(|value| self.optimize_expression(value))
        } else {
            result
        }
    }

    pub(super) fn can_inline_function(&'a self, function: &Function) -> bool {
        let mut found_sub_functions = HashSet::new();
        for token in &function.function_body {
            if token.t_type == TokenType::Expression
                && (self
                    .functions
                    .read()
                    .iter()
                    .any(|v| v.name.bytes == token.bytes)
                    || self
                        .inline_functions
                        .read()
                        .iter()
                        .any(|v| v.name.bytes == token.bytes))
            {
                found_sub_functions.insert(&token.bytes);
            }
        }
        found_sub_functions.is_empty()
    }
}
