use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::{Compiler, Constant, Function, UnparsedCondition, classic};
use crate::constants::COMPAT_CHIA;
use num_bigint::BigInt;
use std::borrow::Cow;
use std::fs;
use std::io::{Error, ErrorKind};
use std::mem::take;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::vec::IntoIter;

pub fn parse_assign_pattern<'a>(
    token_stream: &mut IntoIter<Token<'a>>,
    mut path: BigInt,
    bindings: &mut Vec<(Token<'a>, BigInt)>,
) -> Result<(), Error> {
    let token = token_stream.next().ok_or(Error::new(
        ErrorKind::UnexpectedEof,
        "Expected assign pattern",
    ))?;
    match token.t_type {
        TokenType::Expression => {
            if token.bytes.as_ref() == b"@" {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Expected (@ name pattern) for a capture",
                ));
            }
            if crate::clvm::assemble::handle_int(&token.bytes).is_some()
                || token.bytes.starts_with(b"\"")
                || token.bytes.starts_with(b"0x")
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected assign binding name",
                ));
            }
            bindings.push((token, path));
        }
        TokenType::StartCons => {
            if token_stream
                .as_slice()
                .first()
                .is_some_and(|token| token.bytes.as_ref() == b"@")
            {
                token_stream.next();
                let name = token_stream.next().ok_or(Error::new(
                    ErrorKind::UnexpectedEof,
                    "Expected capture name",
                ))?;
                if name.t_type != TokenType::Expression || name.bytes.as_ref() == b"@" {
                    return Err(Error::new(ErrorKind::InvalidInput, "Expected capture name"));
                }
                bindings.push((name, path.clone()));
                parse_assign_pattern(token_stream, path, bindings)?;
                if token_stream.next().map(|token| token.t_type) != Some(TokenType::EndCons) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected end of capture pattern",
                    ));
                }
                return Ok(());
            }
            let mut found_first = false;
            loop {
                match token_stream.as_slice().first().map(|v| v.t_type) {
                    Some(TokenType::EndCons) => {
                        token_stream.next();
                        break;
                    }
                    Some(TokenType::DotCons) if found_first => {
                        token_stream.next();
                        parse_assign_pattern(token_stream, path, bindings)?;
                        if token_stream.next().map(|v| v.t_type) != Some(TokenType::EndCons) {
                            return Err(Error::new(
                                ErrorKind::InvalidInput,
                                "Expected end of assign pattern",
                            ));
                        }
                        break;
                    }
                    None => {
                        return Err(Error::new(
                            ErrorKind::UnexpectedEof,
                            "Unclosed assign pattern",
                        ));
                    }
                    _ => {
                        let bit = BigInt::from(1u8) << (path.bits() - 1);
                        parse_assign_pattern(token_stream, &path + &bit, bindings)?;
                        path += bit << 1;
                        found_first = true;
                    }
                }
            }
        }
        _ => {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected assign pattern",
            ));
        }
    }
    Ok(())
}

pub fn parse_function<'a>(
    conditions_queue: &mut IntoIter<Token<'a>>,
    classic: bool,
) -> Result<Function<'a>, Error> {
    let function_name = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    if function_name.t_type != TokenType::Expression {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Expected function name",
        ));
    }
    let argument_pattern = read_form(conditions_queue)?;
    let mut arguments = argument_pattern.clone().into_iter();
    let pattern = arguments.next().unwrap();
    if classic && pattern.t_type == TokenType::Expression {
        return Ok(Function {
            name: function_name,
            argument_names: vec![pattern.clone()],
            has_rest_arg: true,
            function_body: conditions_queue.collect(),
            argument_pattern: vec![pattern],
            generated: false,
        });
    }
    if pattern.t_type != TokenType::StartCons {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "Expected function argument list",
        ));
    }
    let mut function_args = vec![];
    let mut has_rest_arg = false;
    let mut nested = false;
    loop {
        let arg_or_end = arguments.next().ok_or(Error::new(
            ErrorKind::InvalidInput,
            "Unexpected End of Token Stream",
        ))?;
        match arg_or_end.t_type {
            TokenType::Expression => {
                function_args.push(arg_or_end);
            }
            TokenType::EndCons => {
                break;
            }
            TokenType::Comment => {}
            TokenType::StartCons if classic => {
                nested = true;
                let mut depth = 1;
                for token in arguments.by_ref() {
                    match token.t_type {
                        TokenType::StartCons => depth += 1,
                        TokenType::EndCons => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                }
                function_args.push(Token {
                    bytes: Cow::Owned(format!("\0argument_{}", function_args.len()).into_bytes()),
                    t_type: TokenType::Expression,
                    ..arg_or_end
                });
            }
            TokenType::DotCons if !function_args.is_empty() => {
                let rest_arg =
                    arguments
                        .find(|v| v.t_type != TokenType::Comment)
                        .ok_or(Error::new(
                            ErrorKind::UnexpectedEof,
                            "Expected rest argument",
                        ))?;
                if rest_arg.t_type != TokenType::Expression {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected rest argument name",
                    ));
                }
                if arguments
                    .find(|v| v.t_type != TokenType::Comment)
                    .map(|v| v.t_type)
                    != Some(TokenType::EndCons)
                {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected end of argument list",
                    ));
                }
                function_args.push(rest_arg);
                has_rest_arg = true;
                break;
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected Token {arg_or_end:?}"),
                ));
            }
        }
    }
    if nested {
        parse_assign_pattern(
            &mut argument_pattern.clone().into_iter(),
            BigInt::from(1u8),
            &mut vec![],
        )?;
    }
    Ok(Function {
        name: function_name,
        argument_names: function_args,
        has_rest_arg,
        function_body: conditions_queue.collect(),
        argument_pattern: if nested { argument_pattern } else { vec![] },
        generated: false,
    })
}

pub fn parse_constant<'a>(
    conditions_queue: &mut IntoIter<Token<'a>>,
) -> Result<Constant<'a>, Error> {
    let name = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    if name.t_type != TokenType::Expression {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Unexpected Token, Expected Expression Got {:?}",
                name.t_type
            ),
        ));
    }
    let value = read_form(conditions_queue)?;
    let end_cons = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    if end_cons.t_type != TokenType::EndCons {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Unexpected Token, Expected End Cons Got {:?}",
                end_cons.t_type
            ),
        ));
    }
    Ok(Constant { name, value })
}

pub fn parse_include<'a>(
    conditions_queue: &mut IntoIter<Token<'a>>,
    include_dirs: &[&str],
) -> Result<Tokenizer<'a>, Error> {
    Ok(Tokenizer::new(Cow::Owned(read_include(
        conditions_queue,
        include_dirs,
    )?)))
}

pub fn read_include(
    conditions_queue: &mut IntoIter<Token<'_>>,
    include_dirs: &[&str],
) -> Result<Vec<u8>, Error> {
    let name_token = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    if name_token.t_type != TokenType::Expression {
        Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Unexpected Token, Expected Expression Got {:?}",
                name_token.t_type
            ),
        ))
    } else {
        let file_name = std::str::from_utf8(&name_token.bytes)
            .map_err(|e| Error::new(ErrorKind::InvalidInput, e))?;
        let file_name = if file_name.starts_with('"') {
            file_name
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or(Error::new(
                    ErrorKind::InvalidInput,
                    "Unterminated include filename",
                ))?
        } else {
            file_name
        };
        if conditions_queue.next().map(|v| v.t_type) != Some(TokenType::EndCons)
            || conditions_queue.next().is_some()
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected end of include",
            ));
        }
        for include_dir in include_dirs {
            let path = Path::new(include_dir).join(file_name);
            match fs::read(&path) {
                Ok(data) => return Ok(data),
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(
            ErrorKind::NotFound,
            format!("Failed to Find include: {file_name}"),
        ))
    }
}

pub fn read_form<'a>(tokens: &mut IntoIter<Token<'a>>) -> Result<Vec<Token<'a>>, Error> {
    let token = tokens
        .find(|token| token.t_type != TokenType::Comment)
        .ok_or(Error::new(ErrorKind::UnexpectedEof, "Expected Expression"))?;
    let mut depth = usize::from(token.t_type == TokenType::StartCons);
    let mut result = vec![token];
    while depth != 0 {
        let token = tokens
            .next()
            .ok_or(Error::new(ErrorKind::UnexpectedEof, "Unclosed Expression"))?;
        match token.t_type {
            TokenType::Comment => continue,
            TokenType::StartCons => depth += 1,
            TokenType::EndCons => depth -= 1,
            _ => {}
        }
        result.push(token);
    }
    Ok(result)
}

impl<'a> Compiler<'a> {
    pub(super) fn ensure_token(&'a self, t_type: TokenType) -> Result<Token<'a>, Error> {
        while let Some(token) = self.reader.next_token() {
            if token.t_type == TokenType::Comment {
                continue;
            }
            return if token.t_type != t_type {
                Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Unexpected Token, Expected {t_type:?} Got {:?}",
                        token.t_type
                    ),
                ))
            } else {
                Ok(token)
            };
        }
        Err(Error::new(
            ErrorKind::UnexpectedEof,
            format!("Expected {t_type:?}"),
        ))
    }
    pub(super) fn ensure_token_value(
        &'a self,
        t_type: TokenType,
        expected_val: &[u8],
    ) -> Result<Token<'a>, Error> {
        let token = self.ensure_token(t_type)?;
        if token.bytes != expected_val {
            Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Unexpected token value got {token:?}"),
            ))
        } else {
            Ok(token)
        }
    }
    pub(super) fn parse_argument_names(&'a self) -> Result<(), Error> {
        let first = std::iter::from_fn(|| self.reader.next_token())
            .find(|token| token.t_type != TokenType::Comment)
            .ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected module arguments",
            ))?;
        if first.t_type == TokenType::Expression && self.flags & COMPAT_CHIA != 0 {
            self.argument_names.write().push(first.clone());
            *self.argument_pattern.write() = vec![first];
            return Ok(());
        }
        if first.t_type != TokenType::StartCons {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected module arguments",
            ));
        }
        let mut pattern = vec![first];
        let mut depth = 1;
        while let Some(token) = self.reader.next_token() {
            match token.t_type {
                TokenType::StartCons => depth += 1,
                TokenType::EndCons => depth -= 1,
                TokenType::Comment => continue,
                _ => {}
            }
            pattern.push(token);
            if depth == 0 {
                break;
            }
        }
        if depth != 0 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "Unclosed argument list",
            ));
        }
        let nested = pattern[1..pattern.len() - 1]
            .iter()
            .any(|token| matches!(token.t_type, TokenType::StartCons | TokenType::DotCons));
        if nested {
            if self.flags & COMPAT_CHIA == 0 {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Nested module arguments require COMPAT_CHIA",
                ));
            }
            let mut bindings = vec![];
            parse_assign_pattern(
                &mut pattern.clone().into_iter(),
                num_bigint::BigInt::from(1u8),
                &mut bindings,
            )?;
            self.argument_names
                .write()
                .extend(bindings.into_iter().map(|(name, _)| name));
            *self.argument_pattern.write() = pattern;
        } else {
            self.argument_names.write().extend(
                pattern
                    .into_iter()
                    .filter(|token| token.t_type == TokenType::Expression),
            );
        }
        Ok(())
    }
    pub(super) fn parse_conditions(&'a self) -> Result<(), Error> {
        let mut conditions = vec![];
        while let Some(token) = self.reader.next_token() {
            if token.t_type == TokenType::Comment {
                continue;
            }
            if token.t_type == TokenType::StartCons {
                let mut tokens = vec![token];
                let mut depth = 0;
                while let Some(token) = self.reader.next_token() {
                    match token.t_type {
                        TokenType::EndCons => {
                            tokens.push(token);
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        }
                        TokenType::Expression | TokenType::DotCons | TokenType::Comment => {
                            tokens.push(token);
                        }
                        TokenType::StartCons => {
                            tokens.push(token);
                            depth += 1;
                        }
                    }
                }
                let cond = UnparsedCondition { tokens };
                conditions.push(cond);
            } else if token.t_type == TokenType::EndCons {
                match conditions.pop() {
                    Some(entry_node) => {
                        for condition in conditions {
                            self.parse_condition(condition, 0)?
                        }
                        *self.body.lock().as_mut() = entry_node.tokens;
                    }
                    None => {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "Expected At Least 1 Condition",
                        ));
                    }
                }
                return Ok(());
            } else {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected token, Expected Start Cons got {token:?}"),
                ));
            }
        }
        Err(Error::new(ErrorKind::UnexpectedEof, "Expected Start Cons"))
    }
    fn parse_condition(
        &'a self,
        condition: UnparsedCondition<'a>,
        include_depth: usize,
    ) -> Result<(), Error> {
        let mut conditions_queue = condition.tokens.into_iter();
        if conditions_queue.next().map(|token| token.t_type) != Some(TokenType::StartCons) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected declaration list",
            ));
        }
        let operator = conditions_queue.next().ok_or(Error::new(
            ErrorKind::UnexpectedEof,
            "Expected declaration name",
        ))?;
        if operator.t_type != TokenType::Expression {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected declaration name",
            ));
        }
        match operator.bytes.as_ref() {
            b"defmacro" => {
                if self.compiler_version.load(Ordering::Relaxed) >= 25 {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "Macros require classic Chia compatibility or CL21/CL23",
                    ));
                }
                let name = conditions_queue
                    .next()
                    .ok_or(Error::new(ErrorKind::UnexpectedEof, "Expected macro name"))?;
                if name.t_type != TokenType::Expression {
                    return Err(Error::new(ErrorKind::InvalidInput, "Expected macro name"));
                }
                let pattern = read_form(&mut conditions_queue)?;
                let body = read_form(&mut conditions_queue)?;
                if conditions_queue.next().map(|token| token.t_type) != Some(TokenType::EndCons)
                    || conditions_queue.next().is_some()
                {
                    return Err(Error::new(ErrorKind::InvalidInput, "Expected end of macro"));
                }
                self.macros.write().push(classic::Macro {
                    name: name.bytes.into_owned(),
                    pattern: self
                        .process_quoted(&mut pattern.into_iter(), false)
                        .map_err(|e| Error::new(e.kind(), format!("Macro pattern: {e}")))?
                        .to_owned(),
                    body: self
                        .process_quoted(&mut body.into_iter(), false)
                        .map_err(|e| Error::new(e.kind(), format!("Macro body: {e}")))?
                        .to_owned(),
                });
            }
            b"defconstant" => {
                self.constants
                    .write()
                    .push(parse_constant(&mut conditions_queue)?);
            }
            b"embed-file" => {
                let mut conditions_queue = conditions_queue
                    .filter(|v| v.t_type != TokenType::Comment)
                    .collect::<Vec<_>>()
                    .into_iter();
                let name = conditions_queue.next().ok_or(Error::new(
                    ErrorKind::UnexpectedEof,
                    "Expected embedded file name",
                ))?;
                let kind = conditions_queue.next().ok_or(Error::new(
                    ErrorKind::UnexpectedEof,
                    "Expected embedded file kind",
                ))?;
                if name.t_type != TokenType::Expression || kind.t_type != TokenType::Expression {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected embedded file name and kind",
                    ));
                }
                if kind.bytes.as_ref() != b"bin" {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        "Only binary embed-file is supported",
                    ));
                }
                let data = read_include(&mut conditions_queue, self.include_dirs)?;
                self.declaration_order.write().push(name.bytes.clone());
                let value = self.byte_atom(data);
                self.embedded_files.write().push((name, value));
            }
            b"defun" | b"defun-inline" => {
                let function =
                    parse_function(&mut conditions_queue, self.flags & COMPAT_CHIA != 0)?;
                self.reference_names.lock().record_function(&function)?;
                self.declaration_order
                    .write()
                    .push(function.name.bytes.clone());
                if operator.bytes.as_ref() == b"defun-inline" {
                    self.inline_functions.write().push(function);
                } else {
                    self.functions.write().push(function);
                }
            }
            b"include" => self.process_include(conditions_queue, include_depth)?,
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected Expression: {operator:?}"),
                ));
            }
        }
        Ok(())
    }

    fn process_include(
        &'a self,
        conditions_queue: IntoIter<Token<'a>>,
        include_depth: usize,
    ) -> Result<(), Error> {
        let mut conditions_queue = conditions_queue
            .filter(|v| v.t_type != TokenType::Comment)
            .collect::<Vec<_>>()
            .into_iter();
        let name = conditions_queue.as_slice().first().ok_or(Error::new(
            ErrorKind::UnexpectedEof,
            "Expected include name",
        ))?;
        if name.t_type == TokenType::Expression
            && name.bytes.starts_with(b"*")
            && name.bytes.ends_with(b"*")
        {
            return self.parse_sigil(&conditions_queue);
        }
        if include_depth >= 64 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Include nesting limit exceeded",
            ));
        }
        let first = self.declaration_order.read().len();
        let reader = parse_include(&mut conditions_queue, self.include_dirs)?;
        let mut tokens = vec![];
        let mut depth = 0;
        let mut found_start = false;
        let mut found_end = false;
        while let Some(token) = reader.next_token() {
            if token.t_type == TokenType::Comment {
                continue;
            }
            if !found_start {
                if token.t_type != TokenType::StartCons {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected include declaration list",
                    ));
                }
                found_start = true;
                continue;
            }
            if found_end {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unexpected token after include declaration list",
                ));
            }
            match token.t_type {
                TokenType::StartCons => depth += 1,
                TokenType::EndCons if depth == 0 => {
                    found_end = true;
                    continue;
                }
                TokenType::EndCons => depth -= 1,
                _ if depth == 0 => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected include declaration",
                    ));
                }
                _ => {}
            }
            // Included declarations must outlive the temporary reader.
            tokens.push(Token {
                bytes: Cow::Owned(token.bytes.into_owned()),
                index: token.index,
                t_type: token.t_type,
            });
            if depth == 0 {
                if tokens.len() < 3 || tokens[1].t_type != TokenType::Expression {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected include declaration",
                    ));
                }
                self.parse_condition(
                    UnparsedCondition {
                        tokens: take(&mut tokens),
                    },
                    include_depth + 1,
                )?;
            }
        }
        for name in &self.declaration_order.read()[first..] {
            if let Some(function) = self
                .functions
                .read()
                .iter()
                .chain(self.inline_functions.read().iter())
                .find(|function| &function.name.bytes == name)
            {
                self.reference_names.lock().record_function(function)?;
            }
        }
        if !found_end {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "Unclosed include declaration list",
            ));
        }
        Ok(())
    }
}
