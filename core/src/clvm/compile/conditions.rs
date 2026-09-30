use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::{Constant, Function};
use num_bigint::BigInt;
use std::borrow::Cow;
use std::fs;
use std::io::{Error, ErrorKind};
use std::path::Path;
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
