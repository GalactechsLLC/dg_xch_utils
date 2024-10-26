use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::{Constant, Function};
use std::io::{Error, ErrorKind};
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

// pub fn parse_operator<'a>(_condition: Token<'a>, _conditions_queue: &mut IntoIter<Token<'a>>) -> Result<Function<'a>, Error> {
//     todo!()
// }
