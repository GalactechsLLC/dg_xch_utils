mod conditions;
pub mod tokenizer;

use crate::clvm::assemble::keywords::{APPLY, B_KEYWORD_TO_ATOM, CONS, QUOTE};
use crate::clvm::assemble::{handle_bytes, handle_hex, handle_int, handle_quote};
use crate::clvm::casts::bigint_to_bytes;
use crate::clvm::compile::conditions::{parse_constant, parse_function};
use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::program::Program;
use crate::clvm::sexp::{AtomBuf, SExp, NULL};
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::mem::take;
use std::vec::IntoIter;

pub struct UnparsedCondition<'a> {
    tokens: Vec<Token<'a>>,
}
impl<'a> Debug for UnparsedCondition<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.tokens)
    }
}
#[derive(Clone)]
pub struct Function<'a> {
    name: Token<'a>,
    argument_names: Vec<Token<'a>>,
    function_body: Vec<Token<'a>>,
}
impl<'a> Debug for Function<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.name)
    }
}
pub struct Constant<'a> {
    name: Token<'a>,
    value: Token<'a>,
}
impl<'a> Debug for Constant<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Constant({:?}: {:?})", self.name, self.value)
    }
}
#[derive(Debug, Default)]
pub struct Compiler<'a> {
    pub argument_names: Vec<Token<'a>>,
    pub functions: Vec<Function<'a>>,
    pub inline_functions: Vec<Function<'a>>,
    pub constants: Vec<Constant<'a>>,
    pub body: Vec<Token<'a>>,
    pub reader: Tokenizer<'a>,
}
impl<'a> Compiler<'a> {
    pub fn new(source: &'a str) -> Self {
        Self {
            reader: Tokenizer::new(source.trim().as_bytes()),
            ..Default::default()
        }
    }
    pub fn compile(&mut self) -> Result<Program, Error> {
        self.pre_process()?;
        let program = self.process()?;
        self.post_process(program)
    }
    fn pre_process(&mut self) -> Result<(), Error> {
        self.ensure_token(TokenType::StartCons)?;
        self.ensure_token_value(TokenType::Expression, b"mod")?;
        self.parse_argument_names()?;
        self.parse_conditions()
    }
    fn post_process(&mut self, program: Program) -> Result<Program, Error> {
        //TODO Add Post Process Functions
        Ok(program)
    }
    fn process(&mut self) -> Result<Program, Error> {
        let mut output = None;
        let mut iter = take(&mut self.body).into_iter();
        while let Some(token) = iter.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => match output {
                    None => {
                        output = Some(self.process_pair(&mut iter)?);
                    }
                    Some(existing) => {
                        output = Some(existing.cons(&self.process_pair(&mut iter)?));
                    }
                },
                TokenType::Expression => match output {
                    None => {
                        output = Some(self.process_atom(token, &mut iter)?);
                        break;
                    }
                    Some(existing) => {
                        output = Some(existing.cons(&self.process_atom(token, &mut iter)?));
                    }
                },
                TokenType::EndCons => {
                    break;
                }
                TokenType::Comment => {}
            }
        }
        let body = output.ok_or(Error::new(ErrorKind::InvalidData, "No body found"))?;
        if self.functions.is_empty() {
            Ok(body)
        } else {
            Program::from_sexp(
                SExp::Atom(AtomBuf::new(vec![APPLY])).cons(
                    SExp::Atom(AtomBuf::new(vec![QUOTE]))
                        .cons(body.sexp)
                        .cons(self.get_functions_sexp()?),
                ),
            )
        }
    }

    fn process_pair(&mut self, token_stream: &mut IntoIter<Token<'a>>) -> Result<Program, Error> {
        let mut entries = vec![];
        let mut found_end_cons = false;
        while let Some(token) = token_stream.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => {
                    entries.push(self.process_pair(token_stream)?);
                }
                TokenType::EndCons => {
                    found_end_cons = true;
                    break;
                }
                TokenType::Expression => {
                    entries.push(self.process_atom(token, token_stream)?);
                }
                TokenType::Comment => {}
            }
        }
        if entries.is_empty() {
            return if found_end_cons {
                Ok(Program::null())
            } else {
                Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "No closing cons found",
                ))
            };
        }
        if entries.len() == 1 {
            entries.pop().ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected Entry, Length Was Checked",
            ))
        } else if entries.len() == 2 {
            Ok(entries[0].cons(&entries[1]))
        } else {
            let mut prog = None;
            while let Some(next) = entries.pop() {
                match prog {
                    None => {
                        prog = Some(next);
                    }
                    Some(existing) => {
                        let new = next.cons(&existing);
                        prog = Some(new);
                    }
                }
            }
            prog.ok_or(Error::new(ErrorKind::InvalidData, "No body found"))
        }
    }

    fn process_atom(
        &mut self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<Program, Error> {
        let is_function: bool = self.functions.iter().any(|v| v.name.bytes == token.bytes);
        let is_inline: bool = self
            .inline_functions
            .iter()
            .any(|v| v.name.bytes == token.bytes);
        let is_constant: bool = self.constants.iter().any(|v| v.name.bytes == token.bytes);
        let is_arg: bool = self.argument_names.iter().any(|v| v.bytes == token.bytes);
        if is_function {
            self.get_function(token, token_stream)
        } else if is_inline {
            self.get_inline_function(token)
        } else if is_constant {
            self.get_constant(token)
        } else if is_arg {
            self.get_arg(token)
        } else {
            //Real Atom
            todo!()
        }
    }

    fn get_function(
        &mut self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<Program, Error> {
        let (index, function) = self
            .functions
            .iter()
            .enumerate()
            .find(|v| v.1.name.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Function not found"))?;
        let func_pointer = Self::get_function_pointer(index as u8)?;
        let num_args = function.argument_names.len() as u32;
        let args = self.get_function_args(num_args, token_stream)?;
        println!("Args: {args:?}");
        Program::from_sexp(
            SExp::Atom(AtomBuf::new(vec![APPLY])).cons(
                SExp::Atom(AtomBuf::new(vec![func_pointer as u8])).cons(
                    SExp::Atom(AtomBuf::new(vec![CONS]))
                        .cons(
                            SExp::Atom(AtomBuf::new(vec![0x02]))
                                .cons(SExp::Atom(AtomBuf::new(vec![CONS])).cons(args)),
                        )
                        .cons(NULL.clone()),
                ),
            ),
        )
    }

    fn get_function_args(
        &mut self,
        num_args: u32,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<SExp, Error> {
        let mut args = vec![];
        for _ in 0..num_args {
            let token = token_stream.next().ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected Function argument",
            ))?;
            match token.t_type {
                TokenType::StartCons => {
                    args.push(self.process_pair(token_stream)?);
                }
                TokenType::Expression => {
                    args.push(self.process_atom(token, token_stream)?);
                }
                TokenType::DotCons | TokenType::EndCons | TokenType::Comment => {
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        format!("Expected Atm or Pair, Got {token:?}"),
                    ));
                }
            }
        }
        let mut rtn = NULL.clone();
        for arg in args.into_iter().rev() {
            rtn = arg.sexp.cons(rtn);
        }
        Ok(rtn)
    }

    fn get_functions_sexp(&mut self) -> Result<SExp, Error> {
        let mut funcs = vec![];
        for func in self.functions.clone().into_iter() {
            funcs.push(self.get_function_body(func)?);
        }
        let mut rtn = None;
        for arg in funcs.into_iter() {
            match rtn {
                None => rtn = Some(arg),
                Some(r) => {
                    rtn = Some(arg.cons(r));
                }
            }
        }
        Ok(SExp::Atom(AtomBuf::new(vec![CONS])).cons(
            rtn.unwrap_or(NULL.clone())
                .cons(SExp::Atom(AtomBuf::new(vec![1u8]))),
        ))
    }

    fn get_function_body(&mut self, function: Function<'a>) -> Result<SExp, Error> {
        let mut tokens = function.function_body.into_iter();
        let args = &function.argument_names;
        self.process_function_pair(&mut tokens, args)
    }

    fn process_function_pair(
        &mut self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
    ) -> Result<SExp, Error> {
        let mut entries = vec![];
        let mut found_end_cons = false;
        while let Some(token) = token_stream.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => {
                    entries.push(self.process_function_pair(token_stream, function_args)?);
                }
                TokenType::EndCons => {
                    found_end_cons = true;
                    break;
                }
                TokenType::Expression => {
                    entries.push(self.process_function_atom(token, token_stream, function_args)?);
                }
                TokenType::Comment => {}
            }
        }
        if entries.is_empty() {
            return if found_end_cons {
                Ok(NULL.clone())
            } else {
                Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "No closing cons found",
                ))
            };
        }
        if entries.len() == 1 {
            let first = entries
                .pop()
                .ok_or(Error::new(ErrorKind::Other, "Expected 1 Entries Found 0"))?;
            Ok(first)
        } else if entries.len() == 2 {
            let rest = entries
                .pop()
                .ok_or(Error::new(ErrorKind::Other, "Expected 2 Entries Found 0"))?;
            let first = entries
                .pop()
                .ok_or(Error::new(ErrorKind::Other, "Expected 2 Entries Found 1"))?;
            Ok(first.cons(rest))
        } else {
            let mut sexp = None;
            let iter = entries.into_iter().rev();
            for next in iter {
                match sexp {
                    None => {
                        sexp = Some(next);
                    }
                    Some(existing) => {
                        let new = next.cons(existing);
                        sexp = Some(new);
                    }
                }
            }
            let sexp = sexp.ok_or(Error::new(ErrorKind::InvalidData, "No body found"))?;
            let quoted = SExp::Atom(AtomBuf::new(vec![QUOTE])).cons(sexp);
            println!("Function Pair: {:?}", quoted);
            Ok(quoted)
        }
    }

    fn process_function_atom(
        &mut self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
    ) -> Result<SExp, Error> {
        let is_function: bool = self.functions.iter().any(|v| v.name.bytes == token.bytes);
        let is_inline: bool = self
            .inline_functions
            .iter()
            .any(|v| v.name.bytes == token.bytes);
        let is_constant: bool = self.constants.iter().any(|v| v.name.bytes == token.bytes);
        let is_arg: bool = function_args.iter().any(|v| v.bytes == token.bytes);
        if is_function {
            self.get_function(token, token_stream).map(|v| v.sexp)
        } else if is_inline {
            self.get_inline_function(token).map(|v| v.sexp)
        } else if is_constant {
            self.get_constant(token).map(|v| v.sexp)
        } else if is_arg {
            let (index, _) = function_args
                .iter()
                .enumerate()
                .find(|v| v.1.bytes == token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
            let arg_pointer = Self::get_arg_pointer((index + 1) as u8)?;
            Ok(SExp::Atom(AtomBuf::new(vec![arg_pointer as u8])))
        } else if let Some(kw) = B_KEYWORD_TO_ATOM.get(&token.bytes) {
            Ok(SExp::Atom(AtomBuf::new(kw.clone())))
        } else {
            Ok(SExp::Atom(AtomBuf::new(token.bytes.to_vec())))
        }
    }

    fn get_function_pointer(function_index: u8) -> Result<u32, Error> {
        let mut pointer = 1u32;
        for _ in 0..function_index {
            pointer += 1;
            pointer <<= 1;
        }
        pointer <<= 1;
        Ok(pointer)
    }

    fn get_arg_pointer(arg_index: u8) -> Result<u32, Error> {
        let mut pointer = 1u32;
        for _ in 0..arg_index {
            pointer += 1;
            pointer <<= 1;
        }
        pointer += 1;
        Ok(pointer)
    }

    fn get_inline_function(&mut self, _token: Token<'a>) -> Result<Program, Error> {
        todo!()
    }

    fn get_constant(&mut self, token: Token<'a>) -> Result<Program, Error> {
        self.constants
            .iter()
            .find(|v| v.name.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))
            .map(|v| {
                Ok(Program::from_sexp(
                    SExp::Atom(AtomBuf::new(vec![QUOTE])).cons(Self::parse_value(v.value.bytes)?),
                )
                .unwrap())
            })?
    }

    fn parse_value(value: &[u8]) -> Result<SExp, Error> {
        if value.is_empty() {
            Ok(NULL.clone())
        } else {
            match handle_int(value) {
                Some(v) => bigint_to_bytes(&v, true).map(|v| SExp::Atom(AtomBuf::new(v))),
                None => handle_hex(value)?
                    .or_else(|| handle_quote(value).or_else(|| Some(handle_bytes(value))))
                    .ok_or_else(|| {
                        Error::new(
                            ErrorKind::Other,
                            format!("Failed to parse Value: {value:?}"),
                        )
                    }),
            }
        }
    }

    fn get_arg(&mut self, token: Token<'a>) -> Result<Program, Error> {
        let (index, _) = self
            .argument_names
            .iter()
            .enumerate()
            .find(|v| v.1.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
        let arg_pointer = if self.functions.is_empty() {
            Self::get_arg_pointer(index as u8)?
        } else {
            Self::get_arg_pointer((index + 1) as u8)?
        };
        Program::from_sexp(SExp::Atom(AtomBuf::new(vec![arg_pointer as u8])))
    }
    fn ensure_token(&mut self, t_type: TokenType) -> Result<Token<'a>, Error> {
        if let Some(token) = self.reader.next() {
            if token.t_type != t_type {
                Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!(
                        "Unexpected Token, Expected {t_type:?} Got {:?}",
                        token.t_type
                    ),
                ))
            } else {
                Ok(token)
            }
        } else {
            Err(Error::new(
                ErrorKind::UnexpectedEof,
                format!("Expected {t_type:?}"),
            ))
        }
    }
    fn ensure_token_value(
        &mut self,
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
    fn parse_argument_names(&mut self) -> Result<(), Error> {
        self.ensure_token(TokenType::StartCons)?;
        for token in self.reader.by_ref() {
            match token.t_type {
                TokenType::EndCons => {
                    break;
                }
                TokenType::Expression => {
                    self.argument_names.push(token);
                }
                TokenType::StartCons | TokenType::DotCons | TokenType::Comment => {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Unexpected token, Expected Expression",
                    ))
                }
            }
        }
        Ok(())
    }
    fn parse_conditions(&mut self) -> Result<(), Error> {
        let mut conditions = vec![];
        while let Some(token) = self.reader.next() {
            if token.t_type == TokenType::StartCons {
                let mut tokens = vec![token];
                let mut depth = 0;
                for token in self.reader.by_ref() {
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
                            self.parse_condition(condition)?
                        }
                        self.body = entry_node.tokens;
                    }
                    None => {
                        return Err(Error::new(
                            ErrorKind::InvalidInput,
                            "Expected At Least 1 Condition",
                        ))
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
    fn parse_condition(&mut self, condition: UnparsedCondition<'a>) -> Result<(), Error> {
        assert!(condition.tokens.len() >= 2);
        let mut conditions_queue = condition.tokens.into_iter();
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
        let operator = conditions_queue.next().ok_or(Error::new(
            ErrorKind::InvalidInput,
            "Unexpected End of Token Stream",
        ))?;
        assert_eq!(operator.t_type, TokenType::Expression);
        match operator.bytes {
            b"defconstant" => {
                self.constants.push(parse_constant(&mut conditions_queue)?);
            }
            b"defun" => {
                self.functions.push(parse_function(&mut conditions_queue)?);
            }
            b"defun-inline" => {
                self.inline_functions
                    .push(parse_function(&mut conditions_queue)?);
            }
            b"include" => {
                todo!()
            }
            // b"defmacro" => {
            //     todo!()
            // }
            // b"lambda" => {
            //     todo!()
            // }
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected Expression: {:?}", operator),
                ))
            }
        }
        Ok(())
    }
}

#[test]
fn test_defun() {
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defun square (number)
            ;; Returns the number squared.
            (* number number)
        )
        (square num)
    )";
    let mut compiler = Compiler::new(EXAMPLE_CLSP);
    let prog = compiler.compile().unwrap();
    println!("{:?}", prog);
}

#[test]
fn test_constant() {
    const EXAMPLE_CLSP: &str = "
    (mod (num)
        (defconstant NUL_NUM 25)
        (defun mul (number)
            (* NUL_NUM number)
        )
        (mul num)
    )";
    let mut compiler = Compiler::new(EXAMPLE_CLSP);
    let prog = compiler.compile().unwrap();
    println!("{:?}", prog);
}
