use crate::clvm::compile::conditions::{parse_assign_pattern, read_form};
use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::{Compiler, Function, optimize};
use crate::constants::{COMPAT_CHIA, OPT_REFERENCE};
use std::borrow::Cow;
use std::collections::HashSet;
use std::io::{Error, ErrorKind};
use std::sync::atomic::Ordering;

impl<'a> Compiler<'a> {
    fn prepare_expression(
        &'a self,
        tokens: Vec<Token<'a>>,
        argument_pattern: &[Token<'a>],
        generated: &mut Vec<Function<'a>>,
        next_index: &mut usize,
    ) -> Result<Vec<Token<'a>>, Error> {
        let tokens = read_form(&mut tokens.into_iter())?;
        if tokens
            .first()
            .is_none_or(|token| token.t_type != TokenType::StartCons)
            || tokens.len() < 3
        {
            return Ok(tokens);
        }
        let start = tokens[0].clone();
        let end = tokens.last().unwrap().clone();
        let mut operator = tokens[1].clone();
        if matches!(operator.bytes.as_ref(), b"q" | b"quote") {
            return Ok(tokens);
        }
        let mut stream = tokens[2..tokens.len() - 1].iter().cloned();
        let mut forms = vec![];
        while stream.len() != 0 {
            forms.push(read_form(&mut stream)?);
        }
        if operator.bytes.as_ref() == b"lambda" && self.flags & COMPAT_CHIA != 0 {
            if forms.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected lambda arguments and body",
                ));
            }
            let body = forms.pop().unwrap();
            let mut pattern = forms.pop().unwrap();
            if pattern
                .first()
                .is_none_or(|token| token.t_type != TokenType::StartCons)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected lambda argument list",
                ));
            }
            let mut captures = vec![];
            if pattern
                .get(1)
                .is_some_and(|token| token.t_type == TokenType::StartCons)
                && pattern
                    .get(2)
                    .is_some_and(|token| token.bytes.as_ref() == b"&")
            {
                let mut stream = pattern[1..].iter().cloned();
                let capture_form = read_form(&mut stream)?;
                captures.extend_from_slice(&capture_form[2..capture_form.len() - 1]);
                pattern.remove(2);
            } else {
                pattern.insert(1, start.clone());
                pattern.insert(2, end.clone());
            }
            if captures
                .iter()
                .any(|token| token.t_type != TokenType::Expression)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected lambda capture names",
                ));
            }
            let name = Token {
                bytes: Cow::Owned(format!("\0lambda_{:08}", *next_index).into_bytes()),
                ..operator.clone()
            };
            *next_index += 1;
            generated.push(Function {
                name: name.clone(),
                argument_names: vec![],
                has_rest_arg: false,
                function_body: body,
                argument_pattern: pattern,
                generated: false,
            });
            let mut result = vec![
                start.clone(),
                Token {
                    bytes: Cow::Borrowed(b"\0closure"),
                    ..operator.clone()
                },
                name,
                start,
                Token {
                    bytes: Cow::Borrowed(b"list"),
                    ..operator
                },
            ];
            result.extend(captures);
            result.extend([end.clone(), end]);
            return Ok(result);
        }
        if operator.bytes.as_ref() == b"let" && self.flags & COMPAT_CHIA != 0 {
            if forms.len() != 2 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected let bindings and body",
                ));
            }
            let body = forms.pop().unwrap();
            let bindings = forms.pop().unwrap();
            if bindings
                .first()
                .is_none_or(|token| token.t_type != TokenType::StartCons)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected let binding list",
                ));
            }
            let mut stream = bindings[1..bindings.len() - 1].iter().cloned();
            while stream.len() != 0 {
                let binding = read_form(&mut stream)?;
                if binding
                    .first()
                    .is_none_or(|token| token.t_type != TokenType::StartCons)
                {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected let binding pair",
                    ));
                }
                let mut pair = binding[1..binding.len() - 1].iter().cloned();
                forms.push(read_form(&mut pair)?);
                forms.push(read_form(&mut pair)?);
                if pair.len() != 0 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected let binding pair",
                    ));
                }
            }
            forms.push(body);
            operator.bytes = Cow::Borrowed(b"\0let");
        }
        if !matches!(operator.bytes.as_ref(), b"assign" | b"\0assign" | b"\0let") {
            let mut result = vec![start, operator];
            for form in forms {
                result.extend(self.prepare_expression(
                    form,
                    argument_pattern,
                    generated,
                    next_index,
                )?);
            }
            result.push(end);
            return Ok(result);
        }
        if forms.len().is_multiple_of(2) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected assign binding pairs and a body",
            ));
        }
        let body = forms.pop().unwrap();
        if forms.is_empty() {
            return self.prepare_expression(body, argument_pattern, generated, next_index);
        }
        let mut bindings = vec![];
        let mut names = HashSet::new();
        for pair in forms.as_chunks::<2>().0 {
            let mut pattern = pair[0].clone().into_iter();
            let mut args = vec![];
            parse_assign_pattern(&mut pattern, num_bigint::BigInt::from(1u8), &mut args)?;
            if pattern.next().is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unexpected token after assign pattern",
                ));
            }
            for (name, _) in &args {
                if !names.insert(name.bytes.clone()) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Duplicate assign binding",
                    ));
                }
            }
            bindings.push((pair[0].clone(), pair[1].clone(), args));
        }
        if operator.bytes.as_ref() == b"assign" {
            // The modern frontend orders bindings before reversing their renamed forms.
            // Order that reversed sequence again for code generation.
            for pass in 0..2 {
                if pass == 1 {
                    bindings.reverse();
                }
                let mut finished = 0;
                while finished < bindings.len() {
                    let ready = (finished..bindings.len())
                        .filter(|index| {
                            !bindings[*index].1.iter().any(|token| {
                                bindings[finished..].iter().any(|(_, _, args)| {
                                    args.iter().any(|(name, _)| name.bytes == token.bytes)
                                })
                            })
                        })
                        .collect::<Vec<_>>();
                    if ready.is_empty() {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "Cyclic assign bindings",
                        ));
                    }
                    for index in ready {
                        bindings.swap(finished, index);
                        finished += 1;
                    }
                }
            }
        }
        let sorted = bindings;
        // A parallel group ends when a value needs a binding from that group.
        let mut provided = HashSet::new();
        let mut count = 0;
        for (_, value, args) in &sorted {
            if value.iter().any(|token| provided.contains(&token.bytes)) {
                break;
            }
            provided.extend(args.iter().map(|(name, _)| name.bytes.clone()));
            count += 1;
        }
        let mut function_body = body;
        if count < sorted.len() {
            let mut remainder = vec![
                start.clone(),
                Token {
                    bytes: Cow::Borrowed(b"\0assign"),
                    ..operator.clone()
                },
            ];
            for (pattern, value, _) in &sorted[count..] {
                remainder.extend(pattern.clone());
                remainder.extend(value.clone());
            }
            remainder.extend(function_body);
            remainder.push(end.clone());
            function_body = remainder;
        }
        let name = Token {
            bytes: Cow::Owned(format!("\0assign_{:08}", *next_index).into_bytes()),
            index: operator.index,
            t_type: TokenType::Expression,
        };
        *next_index += 1;
        let mut pattern = vec![start.clone()];
        pattern.extend_from_slice(argument_pattern);
        let mut argument_names = vec![name.clone()];
        let mut result = vec![start.clone(), name.clone(), start.clone()];
        result.push(Token {
            bytes: Cow::Borrowed(b"r"),
            ..operator.clone()
        });
        result.push(Token {
            bytes: Cow::Borrowed(b"@*env*"),
            ..operator.clone()
        });
        result.push(end.clone());
        for (binding, value, _) in &sorted[..count] {
            pattern.extend(binding.clone());
            argument_names.push(name.clone());
            result.extend(self.prepare_expression(
                value.clone(),
                argument_pattern,
                generated,
                next_index,
            )?);
        }
        pattern.push(end.clone());
        result.push(end);
        generated.push(Function {
            name,
            argument_names,
            has_rest_arg: false,
            function_body,
            argument_pattern: pattern,
            generated: true,
        });
        Ok(result)
    }

    pub(super) fn prepare_assigns(&'a self) -> Result<(), Error> {
        let start = Token {
            bytes: Cow::Borrowed(b"("),
            index: 0,
            t_type: TokenType::StartCons,
        };
        let end = Token {
            bytes: Cow::Borrowed(b")"),
            index: 0,
            t_type: TokenType::EndCons,
        };
        let mut functions = self.functions.read().clone();
        functions.extend(self.inline_functions.read().clone());
        functions.sort_by_key(|f| {
            self.declaration_order
                .read()
                .iter()
                .position(|name| name == &f.name.bytes)
        });
        if self.opt_level == OPT_REFERENCE
            && self.flags & COMPAT_CHIA != 0
            && self.compiler_version.load(Ordering::Relaxed) >= 23
        {
            for function in &mut functions {
                function.function_body = optimize::share_expressions(
                    function.function_body.clone(),
                    &self.reference_names.lock().arguments[function.name.bytes.as_ref()],
                )?;
            }
        }
        let mut next_index = 0;
        let mut index = 0;
        while index < functions.len() {
            let function = &functions[index];
            let pattern = if function.argument_pattern.is_empty() {
                let mut pattern = vec![start.clone()];
                for (i, arg) in function.argument_names.iter().enumerate() {
                    if function.has_rest_arg && i + 1 == function.argument_names.len() {
                        pattern.push(Token {
                            bytes: Cow::Borrowed(b"."),
                            index: 0,
                            t_type: TokenType::DotCons,
                        });
                    }
                    pattern.push(arg.clone());
                }
                pattern.push(end.clone());
                pattern
            } else {
                function.argument_pattern.clone()
            };
            let mut generated = vec![];
            let body = self.prepare_expression(
                function.function_body.clone(),
                &pattern,
                &mut generated,
                &mut next_index,
            )?;
            functions[index].function_body = body;
            {
                let mut order = self.declaration_order.write();
                let position = order
                    .iter()
                    .position(|name| name == &functions[index].name.bytes)
                    .unwrap()
                    + usize::from(!functions[index].name.bytes.starts_with(b"\0lambda_"));
                order.splice(
                    position..position,
                    generated.iter().map(|f| f.name.bytes.clone()),
                );
            }
            index += 1;
            functions.splice(index..index, generated);
        }
        let mut pattern = self.argument_pattern.read().clone();
        if pattern.is_empty() {
            pattern.push(start);
            pattern.extend(self.argument_names.read().clone());
            pattern.push(end);
        }
        let mut generated = vec![];
        let body = self.body.lock().clone();
        *self.body.lock() =
            self.prepare_expression(body, &pattern, &mut generated, &mut next_index)?;
        // Process helpers introduced by the module body in the same order.
        let mut index = 0;
        while index < generated.len() {
            let function = generated[index].clone();
            let mut nested = vec![];
            generated[index].function_body = self.prepare_expression(
                function.function_body,
                &function.argument_pattern,
                &mut nested,
                &mut next_index,
            )?;
            index += 1;
            generated.splice(index..index, nested);
        }
        self.declaration_order
            .write()
            .extend(generated.iter().map(|f| f.name.bytes.clone()));
        functions.extend(generated);
        let inline_names: HashSet<_> = self
            .inline_functions
            .read()
            .iter()
            .map(|f| f.name.bytes.clone())
            .collect();
        self.optimize_assigns(&functions, inline_names)
    }
}
