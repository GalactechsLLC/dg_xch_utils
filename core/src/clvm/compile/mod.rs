pub mod conditions;
pub mod tests;
pub mod tokenizer;
pub mod utils;

use crate::clvm::assemble::assemble_text;
use crate::clvm::compile::conditions::{parse_constant, parse_function, parse_include};
use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::utils::{
    concat_args, get_arg_pointer, get_const_pointer, get_function_pointer, parse_value,
};
use crate::clvm::program::Program;
use crate::clvm::sexp::{IntoSExp, SExp};
use crate::constants::{
    APPLY_SEXP, B_KEYWORD_TO_SEXP, CONS_SEXP, INLINE_CONSTS, INLINE_DEFUNS, NULL_SEXP, QUOTE_SEXP,
};
use parking_lot::{Mutex, RwLock};
use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::mem::take;
use std::sync::atomic::{AtomicBool, Ordering};
use std::vec::IntoIter;

pub struct UnparsedCondition<'a> {
    tokens: Vec<Token<'a>>,
}
impl Debug for UnparsedCondition<'_> {
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
impl Debug for Function<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.name)
    }
}
pub struct Constant<'a> {
    name: Token<'a>,
    value: Token<'a>,
}
impl Debug for Constant<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Constant({:?}: {:?})", self.name, self.value)
    }
}

#[derive(Debug, Default)]
pub struct Compiler<'a> {
    pub argument_names: RwLock<Vec<Token<'a>>>,
    pub functions: RwLock<Vec<Function<'a>>>,
    pub inline_functions: RwLock<Vec<Function<'a>>>,
    pub constants: RwLock<Vec<Constant<'a>>>,
    pub body: Mutex<Vec<Token<'a>>>,
    pub reader: Tokenizer<'a>,
    pub include_dirs: &'a [&'a str],
    pub flags: u32,
    pub opt_level: u8,
    pub in_nested: AtomicBool,
}
impl<'a> Compiler<'a> {
    pub fn new(
        source: Cow<'a, [u8]>,
        flags: u32,
        opt_level: u8,
        include_dirs: &'a [&'a str],
    ) -> Self {
        Self {
            reader: Tokenizer::new(source),
            flags,
            opt_level,
            include_dirs,
            ..Default::default()
        }
    }
    pub fn compile(&'a self) -> Result<Program, Error> {
        self.pre_process()?;
        let program = self.process()?;
        self.post_process(program)
    }
    fn pre_process(&'a self) -> Result<(), Error> {
        self.ensure_token(TokenType::StartCons)?;
        self.ensure_token_value(TokenType::Expression, b"mod")?;
        self.parse_argument_names()?;
        self.parse_conditions()?;
        if self.flags & INLINE_DEFUNS == INLINE_DEFUNS {
            let funcs: Vec<Function<'a>> = take(self.functions.write().as_mut());
            let (defun, inline) =
                funcs
                    .into_iter()
                    .fold((vec![], vec![]), |(mut defun, mut inline), func| {
                        if self.can_inline_function(&func) {
                            inline.push(func);
                        } else {
                            defun.push(func);
                        }
                        (defun, inline)
                    });
            *self.functions.write().as_mut() = defun;
            self.inline_functions.write().extend(inline);
        }
        Ok(())
    }
    fn post_process(&'a self, program: Program) -> Result<Program, Error> {
        assemble_text(&format!("{program:?}")).map(|v| v.to_program())
    }
    fn process(&'a self) -> Result<Program, Error> {
        let mut output = None;
        let body: Vec<Token<'a>> = take(self.body.lock().as_mut());
        let mut iter = body.into_iter();
        while let Some(token) = iter.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => match output {
                    None => {
                        output = Some(self.process_pair(&mut iter)?);
                    }
                    Some(existing) => {
                        output = Some(existing.cons(self.process_pair(&mut iter)?));
                    }
                },
                TokenType::Expression => match output {
                    None => {
                        output = Some(self.process_atom(token, &mut iter)?);
                        break;
                    }
                    Some(existing) => {
                        output = Some(existing.cons(self.process_atom(token, &mut iter)?));
                    }
                },
                TokenType::EndCons => {
                    break;
                }
                TokenType::Comment => {}
            }
        }
        let body = output.ok_or(Error::new(ErrorKind::InvalidData, "No body found"))?;
        if self.functions.read().is_empty() {
            Program::from_sexp(body)
        } else {
            Program::from_sexp(
                APPLY_SEXP.clone().cons(
                    QUOTE_SEXP
                        .clone()
                        .cons(body)
                        .cons(self.get_program_args_sexp()?),
                ),
            )
        }
    }

    fn create_pair_sexp(&self, mut entries: Vec<SExp>, found_end: bool) -> Result<SExp, Error> {
        if entries.is_empty() {
            return if found_end {
                Ok(NULL_SEXP.clone())
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
            let rest = entries.pop().ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected Entry, Length Was Checked",
            ))?;
            let first = entries.pop().ok_or(Error::new(
                ErrorKind::UnexpectedEof,
                "Expected Entry, Length Was Checked",
            ))?;
            Ok(first.cons(rest))
        } else {
            concat_args(entries)
        }
    }

    fn process_pair(&'a self, token_stream: &mut IntoIter<Token<'a>>) -> Result<SExp, Error> {
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
        self.create_pair_sexp(entries, found_end_cons)
    }

    fn process_atom(
        &'a self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<SExp, Error> {
        if self
            .functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_function(token, token_stream)
        } else if self
            .inline_functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_inline_function(token, token_stream)
        } else if self
            .constants
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_constant(token)
        } else if self
            .argument_names
            .read()
            .iter()
            .any(|v| v.bytes == token.bytes)
        {
            self.get_arg(token)
        } else {
            Ok(
                if let Some(kw) = B_KEYWORD_TO_SEXP.get(token.bytes.as_ref()) {
                    kw.clone()
                } else {
                    QUOTE_SEXP.clone().cons(parse_value(token.bytes.as_ref())?)
                },
            )
        }
    }

    fn get_function(
        &'a self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<SExp, Error> {
        let (index, num_args) = self
            .functions
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.name.bytes == token.bytes)
            .map(|v| (v.0, v.1.argument_names.len() as u32))
            .ok_or(Error::new(ErrorKind::InvalidData, "Function not found"))?;
        let constants_count =
            self.constants.read().len() * (self.flags & INLINE_CONSTS != INLINE_CONSTS) as usize;
        let func_pointer =
            get_function_pointer(index as u8, constants_count, self.functions.read().len())?;
        let mut i_set_the_value = false;
        if !self.in_nested.load(Ordering::Relaxed) {
            self.in_nested.store(true, Ordering::Relaxed);
            i_set_the_value = true;
        }
        let args = self.get_function_args(num_args, token_stream)?;
        if i_set_the_value {
            self.in_nested.store(false, Ordering::Relaxed);
        }
        Ok(APPLY_SEXP
            .clone()
            .cons((func_pointer as u8).to_sexp().cons({
                let val = CONS_SEXP
                    .clone()
                    .cons(APPLY_SEXP.clone().cons(CONS_SEXP.clone().cons(
                        if self.in_nested.load(Ordering::Relaxed) {
                            args
                        } else {
                            args.cons(NULL_SEXP.clone())
                        },
                    )));
                if self.in_nested.load(Ordering::Relaxed) {
                    val
                } else {
                    val.cons(NULL_SEXP.clone())
                }
            })))
    }

    fn get_function_args(
        &'a self,
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
        if args.len() == 1 {
            Ok(args.pop().expect("Expected at least one argument"))
        } else if args.len() == 2 {
            let rest = args.pop().expect("Expected at least two arguments");
            let first = args.pop().expect("Expected at least two arguments");
            Ok(first.cons(rest))
        } else {
            concat_args(args)
        }
    }

    fn get_program_args_sexp(&'a self) -> Result<SExp, Error> {
        let mut entries = vec![];
        for func in self.functions.read().clone().into_iter() {
            entries.push(self.get_function_body(func)?);
        }
        if self.flags & INLINE_CONSTS != INLINE_CONSTS {
            for constant in self.constants.read().iter().rev() {
                entries.push(parse_value(constant.value.bytes.as_ref())?);
            }
        }
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
        Ok(CONS_SEXP
            .clone()
            .cons(rtn.unwrap_or(NULL_SEXP.clone()).cons(QUOTE_SEXP.clone())))
    }

    fn get_function_body(&'a self, function: Function<'a>) -> Result<SExp, Error> {
        let mut tokens = function.function_body.into_iter();
        let args = &function.argument_names;
        self.process_function_pair(&mut tokens, args, 0)
    }

    fn process_function_pair(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        depth: u32,
    ) -> Result<SExp, Error> {
        let mut entries = vec![];
        let mut found_end_cons = false;
        while let Some(token) = token_stream.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => {
                    let pair =
                        self.process_function_pair(token_stream, function_args, depth + 1)?;
                    if depth == 1 {
                        entries.push(pair.cons(NULL_SEXP.clone()));
                    } else {
                        entries.push(pair);
                    }
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
        self.create_pair_sexp(entries, found_end_cons)
    }

    fn process_function_atom(
        &'a self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
    ) -> Result<SExp, Error> {
        if self
            .functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_function(token, token_stream)
        } else if self
            .inline_functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_inline_function(token, token_stream)
        } else if self
            .constants
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_constant(token)
        } else if function_args.iter().any(|v| v.bytes == token.bytes) {
            let (index, _) = function_args
                .iter()
                .enumerate()
                .find(|v| v.1.bytes == token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
            let arg_pointer = get_arg_pointer((index + 1) as u8)?;
            Ok((arg_pointer as u8).to_sexp())
        } else if let Some(kw) = B_KEYWORD_TO_SEXP.get(&token.bytes.as_ref()) {
            Ok(kw.clone())
        } else {
            Ok(QUOTE_SEXP.clone().cons(parse_value(token.bytes.as_ref())?))
        }
    }

    fn get_inline_function(
        &'a self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
    ) -> Result<SExp, Error> {
        let cloned_func = self
            .inline_functions
            .read()
            .iter()
            .find(|v| v.name.bytes == token.bytes)
            .cloned();
        match cloned_func {
            Some(func) => {
                if self.can_inline_function(&func) {
                    let mut args = vec![];
                    for _ in 0..func.argument_names.len() {
                        if let Some(token) = token_stream.next() {
                            match token.t_type {
                                TokenType::StartCons | TokenType::DotCons => {
                                    args.push(self.process_pair(token_stream)?);
                                }
                                TokenType::EndCons => {
                                    break;
                                }
                                TokenType::Expression => {
                                    args.push(self.process_atom(token, token_stream)?);
                                }
                                TokenType::Comment => {}
                            }
                        } else {
                            return Err(Error::new(
                                ErrorKind::InvalidData,
                                "Failed to parse function arguments",
                            ));
                        }
                    }
                    let mut body_tokens = func.function_body.into_iter();
                    let func_body = self.process_inline_function_pair(
                        &mut body_tokens,
                        &func.argument_names,
                        &args,
                        0,
                    )?;
                    println!("Func Body: {func_body:?}");
                    Ok(func_body)
                } else {
                    Err(Error::new(
                        ErrorKind::InvalidData,
                        "Unable to Inline Function Marked as Inline",
                    ))
                }
            }
            None => Err(Error::new(ErrorKind::InvalidData, "Inline Func not found")),
        }
    }

    fn process_inline_function_pair(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp],
        depth: u32,
    ) -> Result<SExp, Error> {
        let mut entries = vec![];
        let mut found_end_cons = false;
        while let Some(token) = token_stream.next() {
            match token.t_type {
                TokenType::StartCons | TokenType::DotCons => {
                    let pair = self.process_inline_function_pair(
                        token_stream,
                        function_args,
                        mapped_args,
                        depth + 1,
                    )?;
                    if depth == 1 {
                        entries.push(pair.cons(NULL_SEXP.clone()));
                    } else {
                        entries.push(pair);
                    }
                }
                TokenType::EndCons => {
                    found_end_cons = true;
                    break;
                }
                TokenType::Expression => {
                    entries.push(self.process_inline_function_atom(
                        token,
                        token_stream,
                        function_args,
                        mapped_args,
                    )?);
                }
                TokenType::Comment => {}
            }
        }
        self.create_pair_sexp(entries, found_end_cons)
    }

    fn process_inline_function_atom(
        &'a self,
        token: Token<'a>,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp],
    ) -> Result<SExp, Error> {
        if self
            .functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_function(token, token_stream)
        } else if self
            .inline_functions
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_inline_function(token, token_stream)
        } else if self
            .constants
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            self.get_constant(token)
        } else if function_args.iter().any(|v| v.bytes == token.bytes) {
            let (index, _) = function_args
                .iter()
                .enumerate()
                .find(|v| v.1.bytes == token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
            Ok(mapped_args[index].clone())
        } else if let Some(kw) = B_KEYWORD_TO_SEXP.get(token.bytes.as_ref()) {
            Ok(kw.clone())
        } else {
            Ok(QUOTE_SEXP.clone().cons(parse_value(token.bytes.as_ref())?))
        }
    }

    fn can_inline_function(&'a self, function: &Function) -> bool {
        let mut found_sub_functions = HashSet::new();
        for token in &function.function_body {
            if token.t_type == TokenType::Expression
                && self
                    .functions
                    .read()
                    .iter()
                    .any(|v| v.name.bytes == token.bytes)
            {
                found_sub_functions.insert(&token.bytes);
            }
        }
        found_sub_functions.is_empty()
    }

    fn get_constant(&'a self, token: Token<'a>) -> Result<SExp, Error> {
        let (index, _) = self
            .constants
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.name.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
        if self.flags & INLINE_CONSTS == 1 {
            self.constants
                .read()
                .iter()
                .find(|v| v.name.bytes == token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))
                .map(|v| {
                    Ok(QUOTE_SEXP
                        .clone()
                        .cons(parse_value(v.value.bytes.as_ref())?))
                })?
        } else {
            let const_pointer = get_const_pointer(index as u8)?;
            Ok((const_pointer as u8).to_sexp())
        }
    }

    fn get_arg(&'a self, token: Token<'a>) -> Result<SExp, Error> {
        let (index, _) = self
            .argument_names
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
        let arg_pointer = if self.functions.read().is_empty() {
            get_arg_pointer(index as u8)?
        } else {
            get_arg_pointer((index + !self.functions.read().is_empty() as usize) as u8)?
        };
        Ok((arg_pointer as u8).to_sexp())
    }
    fn ensure_token(&'a self, t_type: TokenType) -> Result<Token<'a>, Error> {
        if let Some(token) = self.reader.next_token() {
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
    fn parse_argument_names(&'a self) -> Result<(), Error> {
        self.ensure_token(TokenType::StartCons)?;
        while let Some(token) = self.reader.next_token() {
            match token.t_type {
                TokenType::EndCons => {
                    break;
                }
                TokenType::Expression => {
                    self.argument_names.write().push(token);
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
    fn parse_conditions(&'a self) -> Result<(), Error> {
        let mut conditions = vec![];
        while let Some(token) = self.reader.next_token() {
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
                            self.parse_condition(condition)?
                        }
                        *self.body.lock().as_mut() = entry_node.tokens;
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
    fn parse_condition(&'a self, condition: UnparsedCondition<'a>) -> Result<(), Error> {
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
        match operator.bytes.as_ref() {
            b"defconstant" => {
                self.constants
                    .write()
                    .push(parse_constant(&mut conditions_queue)?);
            }
            b"defun" => {
                self.functions
                    .write()
                    .push(parse_function(&mut conditions_queue)?);
            }
            b"defun-inline" => {
                self.inline_functions
                    .write()
                    .push(parse_function(&mut conditions_queue)?);
            }
            b"include" => {
                let _results = parse_include(&mut conditions_queue, &[])?;
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
                    format!("Unexpected Expression: {operator:?}"),
                ))
            }
        }
        Ok(())
    }
}
