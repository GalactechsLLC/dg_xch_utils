use std::borrow::Cow;
use std::fs;
use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::{Constant, Function};
use std::io::{Error, ErrorKind};
use std::path::Path;
use std::vec::IntoIter;

pub fn parse_function<'a>(
    conditions_queue: &mut IntoIter<Token<'a>>,
) -> Result<Function<'a>, Error> {
    let function_name = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    assert_eq!(function_name.t_type, TokenType::Expression);
    assert_eq!(
        conditions_queue
            .next()
            .ok_or(Error::new(
                ErrorKind::InvalidInput,
                "Unexpected End of Token Stream"
            ))?
            .t_type,
        TokenType::StartCons
    );
    let mut function_args = vec![];
    loop {
        let arg_or_end = conditions_queue.next().ok_or(Error::new(
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
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected Token {arg_or_end:?}"),
                ))
            }
        }
    }
    Ok(Function {
        name: function_name,
        argument_names: function_args,
        function_body: conditions_queue.collect(),
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
    let value = conditions_queue.next().ok_or(Error::new(
        ErrorKind::InvalidInput,
        "Unexpected End of Token Stream",
    ))?;
    if value.t_type != TokenType::Expression {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "Unexpected Token, Expected Expression Got {:?}",
                value.t_type
            ),
        ));
    }
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
        let file_name = String::from_utf8_lossy(&name_token.bytes);
        for include_dir in include_dirs {
            let path = Path::new(include_dir).join(file_name.as_ref());
            if path.exists() {
                let data = fs::read_to_string(&path).unwrap();
                return Ok(Tokenizer::new(Cow::Owned(data.trim().as_bytes().to_vec())));
            }
        }
        Err(Error::new(
            ErrorKind::NotFound,
            format!("Failed to Find include: {file_name}"),
        ))
    }

}
