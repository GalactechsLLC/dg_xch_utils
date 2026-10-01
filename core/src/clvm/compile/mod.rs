mod classic;
mod compat;
pub mod conditions;
mod functions;
mod lowering;
mod optimize;
pub mod tokenizer;
pub mod utils;

use crate::clvm::compile::conditions::parse_assign_pattern;
use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::utils::{
    concat_args, eval_constant, get_arg_pointer, get_const_pointer, get_function_pointer,
    parse_value,
};
use crate::clvm::program::Program;
use crate::clvm::sexp::SExp;
pub use crate::constants::{
    APPLY_SEXP, B_KEYWORD_TO_SEXP, COMPAT_CHIA, CONS_SEXP, INLINE_CONSTS, INLINE_DEFUNS,
    NESTED_ASSIGN, NULL_SEXP, OPT_COST, OPT_DEFAULT, OPT_REFERENCE, OPT_SIZE, QUOTE_SEXP,
};
use parking_lot::{Mutex, RwLock};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Formatter};
use std::io::{Error, ErrorKind};
use std::mem::take;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    has_rest_arg: bool,
    function_body: Vec<Token<'a>>,
    argument_pattern: Vec<Token<'a>>,
    generated: bool,
}
impl Debug for Function<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.name)
    }
}
pub struct Constant<'a> {
    name: Token<'a>,
    value: Vec<Token<'a>>,
}
impl Debug for Constant<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Constant({:?}: {:?})", self.name, self.value)
    }
}

#[derive(Debug, Default)]
pub struct Compiler<'a> {
    pub argument_names: RwLock<Vec<Token<'a>>>,
    argument_pattern: RwLock<Vec<Token<'a>>>,
    macros: RwLock<Vec<classic::Macro>>,
    reference_names: Mutex<optimize::ReferenceNames>,
    pub functions: RwLock<Vec<Function<'a>>>,
    pub inline_functions: RwLock<Vec<Function<'a>>>,
    pub constants: RwLock<Vec<Constant<'a>>>,
    // Retain byte atoms by identity: numbers with the same bytes have a different size score.
    byte_atoms: Mutex<HashMap<usize, Arc<Vec<u8>>>>,
    pub declaration_order: RwLock<Vec<Cow<'a, [u8]>>>,
    pub table_names: RwLock<Vec<Cow<'a, [u8]>>>,
    pub embedded_files: RwLock<Vec<(Token<'a>, SExp<'static>)>>,
    pub body: Mutex<Vec<Token<'a>>>,
    pub reader: Tokenizer<'a>,
    pub include_dirs: &'a [&'a str],
    pub flags: u32,
    pub opt_level: u8,
    pub in_nested: AtomicBool,
    pub compiler_version: AtomicU8,
    pub inline_stack: Mutex<Vec<Cow<'a, [u8]>>>,
    inline_scope: Mutex<Vec<(Token<'static>, SExp<'static>)>>,
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
            flags: flags
                | if opt_level == OPT_COST {
                    NESTED_ASSIGN
                } else {
                    0
                },
            opt_level,
            include_dirs,
            ..Default::default()
        }
    }
    pub fn compile(&'a self) -> Result<Program<'a>, Error> {
        if self.opt_level > OPT_COST {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Unsupported optimization level",
            ));
        }
        self.pre_process()?;
        let program = self.process()?;
        self.post_process(program)
    }
    fn pre_process(&'a self) -> Result<(), Error> {
        if self.flags & COMPAT_CHIA == 0 {
            // DG uses the modern pipeline without historical dialect quirks.
            self.compiler_version.store(26, Ordering::Relaxed);
        }
        self.ensure_token(TokenType::StartCons)?;
        self.ensure_token_value(TokenType::Expression, b"mod")?;
        self.parse_argument_names()?;
        self.parse_conditions()?;
        if self.compiler_version.load(Ordering::Relaxed) == 0
            && (self.opt_level != OPT_REFERENCE || self.flags != COMPAT_CHIA)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Classic compatibility requires OPT_REFERENCE and no optimization flags",
            ));
        }
        if !self.macros.read().is_empty() {
            if self.compiler_version.load(Ordering::Relaxed) >= 25 {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Macros require classic Chia compatibility or CL21/CL23",
                ));
            }
            let body = classic::expand(self, self.body.lock().clone())
                .map_err(|e| Error::new(e.kind(), format!("Macro expansion in module: {e}")))?;
            *self.body.lock() = body;
            for function in self
                .functions
                .write()
                .iter_mut()
                .chain(self.inline_functions.write().iter_mut())
            {
                function.function_body = classic::expand(self, function.function_body.clone())
                    .map_err(|error| {
                        Error::new(
                            error.kind(),
                            format!(
                                "Macro expansion in {}: {error}",
                                String::from_utf8_lossy(&function.name.bytes)
                            ),
                        )
                    })?;
            }
        }
        if self.compiler_version.load(Ordering::Relaxed) >= 25
            && !self.argument_pattern.read().is_empty()
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Nested module arguments are only supported in classic compatibility",
            ));
        }
        if self.compiler_version.load(Ordering::Relaxed) != 0
            && self
                .constants
                .read()
                .iter()
                .any(|constant| constant.value.len() != 1)
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Compound constants are only supported in classic compatibility",
            ));
        }
        if self.compiler_version.load(Ordering::Relaxed) >= 21 {
            let mut functions = self.functions.read().clone();
            functions.extend(self.inline_functions.read().clone());
            functions.sort_by_key(|function| {
                self.declaration_order
                    .read()
                    .iter()
                    .position(|name| name == &function.name.bytes)
            });
            let mut reference = self.reference_names.lock();
            reference.record_body(&self.body.lock())?;
            reference.finish(&functions)?;
        }
        if self.flags & INLINE_DEFUNS == INLINE_DEFUNS {
            let funcs = self.functions.read().clone();
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
        {
            let mut used: HashSet<_> = self
                .body
                .lock()
                .iter()
                .map(|token| token.bytes.clone())
                .collect();
            loop {
                let count = used.len();
                for function in self
                    .functions
                    .read()
                    .iter()
                    .chain(self.inline_functions.read().iter())
                {
                    if used.contains(&function.name.bytes) {
                        used.extend(
                            function
                                .function_body
                                .iter()
                                .map(|token| token.bytes.clone()),
                        );
                    }
                }
                if used.len() == count {
                    break;
                }
            }
            self.functions
                .write()
                .retain(|function| used.contains(&function.name.bytes));
            self.inline_functions
                .write()
                .retain(|function| used.contains(&function.name.bytes));
            self.embedded_files
                .write()
                .retain(|(name, _)| used.contains(&name.bytes));
            if self.compiler_version.load(Ordering::Relaxed) == 0 {
                self.constants
                    .write()
                    .retain(|constant| used.contains(&constant.name.bytes));
                let mut names = self.table_names.write();
                names.extend(self.functions.read().iter().map(|v| v.name.bytes.clone()));
                names.extend(self.constants.read().iter().map(|v| v.name.bytes.clone()));
                names.extend(self.embedded_files.read().iter().map(|v| v.0.bytes.clone()));
                names.sort();
            }
        }
        if self.compiler_version.load(Ordering::Relaxed) >= 21 && self.flags & NESTED_ASSIGN == 0 {
            self.prepare_assigns()?;
        }
        Ok(())
    }
    fn post_process(&'a self, program: Program<'a>) -> Result<Program<'static>, Error> {
        Ok(program.to_owned())
    }
    fn process(&'a self) -> Result<Program<'a>, Error> {
        let body: Vec<Token<'a>> = self.body.lock().clone();
        let mut argument_names = self.argument_names.read().clone();
        let mut args = argument_names
            .iter()
            .cloned()
            .map(|v| self.get_arg(v))
            .collect::<Result<Vec<_>, _>>()?;
        if !self.argument_pattern.read().is_empty() {
            let mut bindings = vec![];
            let root = if self.functions.read().is_empty() && self.table_names.read().is_empty() {
                1u8
            } else {
                3u8
            };
            parse_assign_pattern(
                &mut self.argument_pattern.read().clone().into_iter(),
                num_bigint::BigInt::from(root),
                &mut bindings,
            )?;
            argument_names = bindings.iter().map(|(name, _)| name.clone()).collect();
            args = bindings
                .iter()
                .map(|(_, path)| self.get_path(path))
                .collect();
        }
        argument_names.push(Token {
            bytes: Cow::Borrowed(b"@*env*"),
            index: 0,
            t_type: TokenType::Expression,
        });
        args.push(
            if self.functions.read().is_empty() && self.table_names.read().is_empty() {
                self.create_pair_sexp(vec![CONS_SEXP.clone(), NULL_SEXP, SExp::from(1u8)])?
            } else {
                SExp::from(1u8)
            },
        );
        let body = self.process_expression(&mut body.into_iter(), &argument_names, &args, 0)?;
        Ok(Program::new(
            if self.functions.read().is_empty() && self.table_names.read().is_empty() {
                body
            } else {
                self.create_pair_sexp(vec![
                    APPLY_SEXP.clone(),
                    QUOTE_SEXP.clone().cons(body),
                    self.get_program_args_sexp()?,
                ])?
            },
        ))
    }

    fn byte_atom(&self, bytes: Vec<u8>) -> SExp<'static> {
        let atom = Arc::new(bytes);
        self.byte_atoms
            .lock()
            .insert(Arc::as_ptr(&atom) as usize, atom.clone());
        SExp::Atom(crate::clvm::sexp::AtomBuf::Owned(atom))
    }

    fn create_pair_sexp(&'a self, mut entries: Vec<SExp<'a>>) -> Result<SExp<'a>, Error> {
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
            for entry in entries.iter_mut().skip(1) {
                if *entry == QUOTE_SEXP.clone().cons(NULL_SEXP) {
                    *entry = NULL_SEXP;
                }
            }
            if entries.len() == 2
                && matches!(&entries[0], SExp::Atom(atom) if matches!(atom.as_ref(), [5] | [6]))
            {
                if let SExp::Pair(pair) = &entries[1] {
                    let first = entries[0] == SExp::from(5u8);
                    if pair.first() == &CONS_SEXP && pair.rest().arg_count_is(2) {
                        let values = pair.rest().ref_list();
                        return Ok(values[usize::from(!first)].to_owned());
                    }
                    if pair.first() == &QUOTE_SEXP {
                        if let SExp::Pair(value) = pair.rest() {
                            let value = if first { value.first() } else { value.rest() };
                            return Ok(if *value == NULL_SEXP {
                                NULL_SEXP
                            } else {
                                QUOTE_SEXP.clone().cons(value.to_owned())
                            });
                        }
                    }
                }
            }
            if (entries.len() == 3 && entries[0] == CONS_SEXP)
                || entries.first() == Some(&SExp::from(crate::constants::SHA256))
            {
                let mut values = vec![];
                for entry in &entries[1..] {
                    match entry {
                        SExp::Pair(pair) if pair.first() == &QUOTE_SEXP => {
                            values.push(pair.rest().to_owned());
                        }
                        value if *value == NULL_SEXP => values.push(NULL_SEXP),
                        _ => break,
                    }
                }
                if entries[0] == CONS_SEXP && values.len() == 2 {
                    let tail = values.pop().unwrap();
                    return Ok(QUOTE_SEXP.clone().cons(values.pop().unwrap().cons(tail)));
                }
                if entries[0] == SExp::from(crate::constants::SHA256)
                    && values.len() + 1 == entries.len()
                    && values.iter().all(|value| matches!(value, SExp::Atom(_)))
                {
                    let mut bytes = vec![];
                    for value in values {
                        bytes.extend_from_slice(value.atom()?.as_ref());
                    }
                    return Ok(QUOTE_SEXP
                        .clone()
                        .cons(SExp::from(crate::utils::hash_256(bytes).to_vec())));
                }
            }
        }
        if self.compiler_version.load(Ordering::Relaxed) == 0
            && matches!(entries.first(), Some(SExp::Atom(atom)) if matches!(atom.as_ref(), [17] | [23]))
        {
            if let Some(value) = eval_constant(&entries) {
                return Ok(if value == NULL_SEXP {
                    NULL_SEXP
                } else {
                    QUOTE_SEXP.clone().cons(value)
                });
            }
        }
        entries.push(NULL_SEXP.clone());
        concat_args(entries)
    }

    fn process_expression(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let token = token_stream
            .find(|v| v.t_type != TokenType::Comment)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, "Expected Expression"))?;
        match token.t_type {
            TokenType::StartCons => {
                self.process_pair(token_stream, function_args, mapped_args, env_depth)
            }
            TokenType::Expression => {
                self.process_atom(token, function_args, mapped_args, env_depth)
            }
            _ => Err(Error::new(ErrorKind::InvalidData, "Expected Atm or Pair")),
        }
    }

    fn process_quoted(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        opcodes: bool,
    ) -> Result<SExp<'a>, Error> {
        let token = token_stream.next().ok_or(Error::new(
            ErrorKind::UnexpectedEof,
            "Expected quoted value",
        ))?;
        match token.t_type {
            TokenType::Expression => {
                if self.compiler_version.load(Ordering::Relaxed) == 0
                    && (opcodes || token.bytes.starts_with(b"#"))
                {
                    return parse_value(token.bytes.as_ref()).map_err(Error::other);
                }
                let value = parse_value(token.bytes.as_ref())?;
                Ok(
                    if crate::clvm::assemble::handle_int(token.bytes.as_ref()).is_none() {
                        let bytes = if token.bytes.starts_with(b"0x")
                            || token.bytes.starts_with(b"0X")
                            || token.bytes.starts_with(b"\"")
                            || token.bytes.starts_with(b"'")
                        {
                            value.atom()?.as_ref().to_vec()
                        } else {
                            token.bytes.to_vec()
                        };
                        self.byte_atom(bytes)
                    } else {
                        value
                    },
                )
            }
            TokenType::StartCons => {
                let mut values = vec![];
                let mut tail = NULL_SEXP;
                loop {
                    match token_stream.as_slice().first().map(|token| token.t_type) {
                        Some(TokenType::EndCons) => {
                            token_stream.next();
                            break;
                        }
                        Some(TokenType::DotCons) if !values.is_empty() => {
                            token_stream.next();
                            tail = self.process_quoted(token_stream, opcodes)?;
                            if token_stream
                                .next()
                                .is_none_or(|token| token.t_type != TokenType::EndCons)
                            {
                                return Err(Error::new(
                                    ErrorKind::InvalidInput,
                                    "Expected closing quote cons",
                                ));
                            }
                            break;
                        }
                        Some(_) => values.push(self.process_quoted(token_stream, opcodes)?),
                        None => {
                            return Err(Error::new(
                                ErrorKind::UnexpectedEof,
                                "No closing quote cons",
                            ));
                        }
                    }
                }
                while let Some(value) = values.pop() {
                    tail = value.cons(tail);
                }
                Ok(tail)
            }
            _ => Err(Error::new(ErrorKind::InvalidInput, "Expected quoted value")),
        }
    }

    fn process_pair(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let operator = token_stream
            .find(|v| v.t_type != TokenType::Comment)
            .ok_or(Error::new(ErrorKind::UnexpectedEof, "Expected Expression"))?;
        if operator.t_type == TokenType::EndCons {
            return Ok(if self.compiler_version.load(Ordering::Relaxed) == 21 {
                QUOTE_SEXP.clone().cons(NULL_SEXP)
            } else {
                NULL_SEXP.clone()
            });
        }
        if operator.t_type != TokenType::Expression {
            return Err(Error::new(ErrorKind::InvalidData, "Expected Operator"));
        }
        if operator.bytes.as_ref() == b"qq" && self.compiler_version.load(Ordering::Relaxed) == 0 {
            let value =
                self.process_quasiquote(token_stream, function_args, mapped_args, env_depth)?;
            if token_stream
                .next()
                .is_none_or(|token| token.t_type != TokenType::EndCons)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected one quasiquote argument",
                ));
            }
            return Ok(value);
        }
        if matches!(operator.bytes.as_ref(), b"q" | b"quote") {
            let is_list = operator.bytes.as_ref() == b"q"
                && token_stream
                    .as_slice()
                    .first()
                    .is_none_or(|token| token.t_type != TokenType::DotCons);
            if is_list {
                let mut tokens = vec![Token {
                    bytes: Cow::Borrowed(b"("),
                    t_type: TokenType::StartCons,
                    ..operator.clone()
                }];
                tokens.extend(take(token_stream));
                *token_stream = tokens.into_iter();
            } else if operator.bytes.as_ref() == b"q" {
                token_stream.next();
            }
            let value = self.process_quoted(token_stream, true)?;
            if !is_list
                && token_stream
                    .next()
                    .is_none_or(|token| token.t_type != TokenType::EndCons)
            {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Expected closing quote",
                ));
            }
            return Ok(if value == NULL_SEXP {
                NULL_SEXP
            } else {
                QUOTE_SEXP.clone().cons(value)
            });
        }
        if operator.bytes.as_ref() == b"assign" {
            return self.process_assign(token_stream, function_args, mapped_args, env_depth);
        }
        let is_scope = operator.bytes.as_ref() == b"r"
            && token_stream
                .as_slice()
                .first()
                .is_some_and(|token| token.bytes.as_ref() == b"@*env*");
        let mut args = vec![];

        loop {
            match token_stream.as_slice().first() {
                Some(token) if token.t_type == TokenType::Comment => {
                    token_stream.next();
                }
                Some(token) if token.t_type == TokenType::EndCons => {
                    token_stream.next();
                    break;
                }
                None => {
                    return Err(Error::new(
                        ErrorKind::UnexpectedEof,
                        "No closing cons found",
                    ));
                }
                _ => {
                    args.push(self.process_expression(
                        token_stream,
                        function_args,
                        mapped_args,
                        env_depth,
                    )?);
                }
            }
        }
        if is_scope
            && args.len() == 1
            && ((self.functions.read().is_empty() && self.table_names.read().is_empty())
                || (self.compiler_version.load(Ordering::Relaxed) == 23
                    && self.inline_stack.lock().is_empty())
                || self.inline_stack.lock().last().is_some_and(|name| {
                    self.inline_functions
                        .read()
                        .iter()
                        .any(|function| &function.name.bytes == name && !function.generated)
                }))
        {
            // Inline environments explicitly contain the argument list after the globals.
            if let SExp::Pair(pair) = &args[0] {
                if pair.first() == &CONS_SEXP {
                    if let SExp::Pair(values) = pair.rest() {
                        if let SExp::Pair(tail) = values.rest() {
                            return Ok(tail.first().to_owned());
                        }
                    }
                }
            }
        }
        if self.compiler_version.load(Ordering::Relaxed) == 23
            && matches!(operator.bytes.as_ref(), b"c" | b"sha256")
        {
            let mut entries = vec![B_KEYWORD_TO_SEXP[operator.bytes.as_ref()].clone()];
            entries.extend(args.clone());
            if let Some(value) = eval_constant(&entries) {
                return Ok(QUOTE_SEXP.clone().cons(value));
            }
        }
        match operator.bytes.as_ref() {
            b"\0closure" if self.flags & COMPAT_CHIA != 0 && args.len() == 2 => {
                let captures = args.pop().unwrap();
                self.curry_value(args.pop().unwrap(), captures)
            }
            b"list" => {
                let mut result = if self.compiler_version.load(Ordering::Relaxed) == 21 {
                    QUOTE_SEXP.clone().cons(NULL_SEXP)
                } else {
                    NULL_SEXP.clone()
                };
                while let Some(arg) = args.pop() {
                    result = self.create_pair_sexp(vec![
                        self.byte_atom(vec![crate::constants::CONS]),
                        arg,
                        result,
                    ])?;
                }
                Ok(result)
            }
            b"if" => {
                if args.len() != 3 {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Expected 3 if arguments",
                    ));
                }
                if self.compiler_version.load(Ordering::Relaxed) >= 23
                    && self.flags & NESTED_ASSIGN == 0
                {
                    while let SExp::Pair(condition) = &args[0] {
                        if condition.first() != &SExp::from(crate::constants::NOT) {
                            break;
                        }
                        let SExp::Pair(value) = condition.rest() else {
                            break;
                        };
                        if value.rest() != &NULL_SEXP {
                            break;
                        }
                        args[0] = value.first().to_owned();
                        args.swap(1, 2);
                    }
                    for branch in &mut args[1..] {
                        if self.opt_level == OPT_REFERENCE
                            && self.flags & COMPAT_CHIA != 0
                            && *branch == NULL_SEXP
                        {
                            *branch = QUOTE_SEXP.clone().cons(NULL_SEXP);
                        }
                    }
                }
                if self.compiler_version.load(Ordering::Relaxed) == 21 {
                    for branch in &mut args[1..] {
                        *branch = self.create_pair_sexp(vec![
                            APPLY_SEXP.clone(),
                            QUOTE_SEXP.clone().cons(branch.clone()),
                            SExp::from(1u8),
                        ])?;
                    }
                }
                let when_false = QUOTE_SEXP.clone().cons(args.pop().unwrap());
                let when_true = QUOTE_SEXP.clone().cons(args.pop().unwrap());
                let condition = args.pop().unwrap();
                let branch = self.create_pair_sexp(vec![
                    SExp::from(crate::constants::IF),
                    condition,
                    when_true,
                    when_false,
                ])?;
                self.create_pair_sexp(vec![APPLY_SEXP.clone(), branch, SExp::from(1u8)])
            }
            _ if self
                .functions
                .read()
                .iter()
                .any(|v| v.name.bytes == operator.bytes) =>
            {
                self.get_function(operator, args, env_depth)
            }
            _ if self
                .inline_functions
                .read()
                .iter()
                .any(|v| v.name.bytes == operator.bytes) =>
            {
                self.get_inline_function(operator, args, function_args, mapped_args, env_depth)
            }
            _ => {
                if matches!(operator.bytes.as_ref(), b"f" | b"r")
                    && args.len() == 1
                    && self.compiler_version.load(Ordering::Relaxed) != 21
                {
                    if self.compiler_version.load(Ordering::Relaxed) >= 21
                        && self.flags & NESTED_ASSIGN == 0
                    {
                        return self.select_path(
                            args.pop().unwrap(),
                            num_bigint::BigInt::from(if operator.bytes.as_ref() == b"f" {
                                2u8
                            } else {
                                3u8
                            }),
                        );
                    }

                    if let SExp::Atom(atom) = &args[0] {
                        let path = num_bigint::BigInt::from_bytes_be(
                            num_bigint::Sign::Plus,
                            atom.as_ref(),
                        );
                        if path.sign() == num_bigint::Sign::Plus {
                            let bit = num_bigint::BigInt::from(1u8) << (path.bits() - 1);
                            let path = path
                                + if operator.bytes.as_ref() == b"f" {
                                    bit
                                } else {
                                    bit << 1
                                };
                            return Ok(self.get_path(&path));
                        }
                    }
                }
                if self.flags & COMPAT_CHIA != 0 {
                    let opcode = match operator.bytes.as_ref() {
                        b"secp256k1_verify" => Some(0x13d61f00u32),
                        b"secp256r1_verify" => Some(0x1c3a8f00u32),
                        _ => None,
                    };
                    if let Some(opcode) = opcode {
                        args.insert(0, SExp::from(opcode));
                        return self.create_pair_sexp(args);
                    }
                }
                let operator = match B_KEYWORD_TO_SEXP.get(operator.bytes.as_ref()) {
                    Some(value) => value.clone(),
                    None => self.process_atom(operator, function_args, mapped_args, env_depth)?,
                };
                args.insert(0, operator);
                self.create_pair_sexp(args)
            }
        }
    }

    fn process_assign(
        &'a self,
        token_stream: &mut IntoIter<Token<'a>>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        let mut forms = vec![];
        let mut tokens = vec![];
        let mut depth = 0;
        let mut found_end = false;
        for token in token_stream.by_ref() {
            match token.t_type {
                TokenType::Comment => continue,
                TokenType::StartCons => depth += 1,
                TokenType::EndCons if depth == 0 => {
                    found_end = true;
                    break;
                }
                TokenType::EndCons => depth -= 1,
                _ => {}
            }
            tokens.push(token);
            if depth == 0 {
                forms.push(take(&mut tokens));
            }
        }
        if !found_end {
            return Err(Error::new(ErrorKind::UnexpectedEof, "Unclosed assign"));
        }
        if forms.len().is_multiple_of(2) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "Expected assign binding pairs and a body",
            ));
        }
        let body = forms.pop().unwrap();
        let mut names = HashSet::new();
        let mut bindings = vec![];
        let mut forms = forms.into_iter();
        while let Some(pattern) = forms.next() {
            let mut pattern = pattern.into_iter();
            let mut pattern_args = vec![];
            parse_assign_pattern(
                &mut pattern,
                num_bigint::BigInt::from(3u8),
                &mut pattern_args,
            )?;
            if pattern.next().is_some() {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "Unexpected token after assign pattern",
                ));
            }
            for (name, _) in &pattern_args {
                if !names.insert(name.bytes.clone()) {
                    return Err(Error::new(
                        ErrorKind::InvalidInput,
                        "Duplicate assign binding",
                    ));
                }
            }
            bindings.push((pattern_args, forms.next().unwrap()));
        }
        let mut sorted_bindings = vec![];
        while !bindings.is_empty() {
            let index = bindings
                .iter()
                .position(|(_, tokens)| {
                    !tokens.iter().any(|token| {
                        token.t_type == TokenType::Expression
                            && bindings.iter().any(|(pattern, _)| {
                                pattern.iter().any(|(name, _)| name.bytes == token.bytes)
                            })
                    })
                })
                .ok_or(Error::new(
                    ErrorKind::InvalidInput,
                    "Cyclic assign bindings",
                ))?;
            sorted_bindings.push(bindings.remove(index));
        }
        // The reference compiler drops bindings that the body does not use.
        let mut used: HashSet<_> = body.iter().map(|v| v.bytes.clone()).collect();
        for (pattern, tokens) in sorted_bindings.iter().rev() {
            if pattern.iter().any(|(name, _)| used.contains(&name.bytes)) {
                used.extend(tokens.iter().map(|v| v.bytes.clone()));
            }
        }
        let mut function_args = function_args.to_vec();
        let mut mapped_args = mapped_args.to_vec();
        let mut values = vec![];
        for (pattern, tokens) in sorted_bindings {
            if !pattern.iter().any(|(name, _)| used.contains(&name.bytes)) {
                continue;
            }
            let value = self.process_expression(
                &mut tokens.into_iter(),
                &function_args,
                &mapped_args,
                env_depth + values.len(),
            )?;
            if self.compiler_version.load(Ordering::Relaxed) >= 21 {
                if let SExp::Atom(atom) = &value {
                    let path =
                        num_bigint::BigInt::from_bytes_be(num_bigint::Sign::Plus, atom.as_ref());
                    if path.sign() == num_bigint::Sign::Plus {
                        for (name, binding_path) in pattern {
                            let offset = (binding_path >> 1usize) - 1u8;
                            let binding_path = &path + (offset << (path.bits() - 1));
                            function_args.insert(0, name);
                            mapped_args.insert(0, SExp::from(&binding_path));
                        }
                        continue;
                    }
                }
            }
            values.push(value);
            // Each binding adds (previous environment . value).
            for arg in &mut mapped_args {
                *arg = if let SExp::Atom(atom) = arg {
                    self.get_path(
                        &(num_bigint::BigInt::from_bytes_be(num_bigint::Sign::Plus, atom.as_ref())
                            << 1usize),
                    )
                } else {
                    self.create_pair_sexp(vec![
                        APPLY_SEXP.clone(),
                        QUOTE_SEXP.clone().cons(arg.clone()),
                        SExp::from(2u8),
                    ])?
                };
            }
            for (name, path) in pattern {
                function_args.insert(0, name);
                mapped_args.insert(0, self.get_path(&path));
            }
        }
        let mut result = self.process_expression(
            &mut body.into_iter(),
            &function_args,
            &mapped_args,
            env_depth + values.len(),
        )?;
        while let Some(value) = values.pop() {
            let env = self.create_pair_sexp(vec![CONS_SEXP.clone(), SExp::from(1u8), value])?;
            result = self.create_pair_sexp(vec![
                APPLY_SEXP.clone(),
                QUOTE_SEXP.clone().cons(result),
                env,
            ])?;
        }
        Ok(result)
    }

    fn process_atom(
        &'a self,
        token: Token<'a>,
        function_args: &[Token<'a>],
        mapped_args: &[SExp<'a>],
        env_depth: usize,
    ) -> Result<SExp<'a>, Error> {
        if token.bytes.as_ref() == b"@" && self.compiler_version.load(Ordering::Relaxed) == 0 {
            return Ok(self.get_path(&(num_bigint::BigInt::from(1u8) << env_depth)));
        }
        if let Some(index) = function_args.iter().position(|v| v.bytes == token.bytes) {
            let value = mapped_args[index].clone();
            // CL25's name lookup truncates paths to 64 bits. CL26 fixed this.
            if self.compiler_version.load(Ordering::Relaxed) == 25
                && self.inline_stack.lock().is_empty()
            {
                if let SExp::Atom(atom) = &value {
                    return Ok(SExp::from(
                        &(atom.as_int() & num_bigint::BigInt::from(u64::MAX)),
                    ));
                }
            }
            Ok(value)
        } else if self
            .constants
            .read()
            .iter()
            .any(|v| v.name.bytes == token.bytes)
        {
            let value = self.get_constant(token)?;
            Ok(if let SExp::Atom(atom) = &value {
                self.get_path(
                    &(num_bigint::BigInt::from_bytes_be(num_bigint::Sign::Plus, atom.as_ref())
                        << env_depth),
                )
            } else {
                value
            })
        } else if self.flags & COMPAT_CHIA != 0
            && self.compiler_version.load(Ordering::Relaxed) >= 21
            && self
                .functions
                .read()
                .iter()
                .any(|function| function.name.bytes == token.bytes)
        {
            let names = self.table_names.read();
            let index = names
                .iter()
                .position(|name| name == &token.bytes)
                .ok_or(Error::new(
                    ErrorKind::InvalidInput,
                    "Function value not in environment",
                ))?;
            self.curry_value(
                self.get_path(&(get_function_pointer(index, 0, names.len(), true)? << env_depth)),
                self.get_path(&(num_bigint::BigInt::from(2u8) << env_depth)),
            )
        } else if token.bytes.as_ref() == b"*chialisp-version*"
            && self.flags & COMPAT_CHIA != 0
            && self.compiler_version.load(Ordering::Relaxed) >= 21
        {
            Ok(QUOTE_SEXP
                .clone()
                .cons(SExp::from(self.compiler_version.load(Ordering::Relaxed))))
        } else if let Some((_, value)) = self
            .embedded_files
            .read()
            .iter()
            .find(|v| v.0.bytes == token.bytes)
        {
            if let Some(index) = self
                .table_names
                .read()
                .iter()
                .position(|name| name == &token.bytes)
            {
                Ok(SExp::from(
                    &(get_function_pointer(index, 0, self.table_names.read().len(), true)?
                        << env_depth),
                ))
            } else {
                Ok(QUOTE_SEXP.clone().cons(value.clone()))
            }
        } else {
            let mut value = parse_value(token.bytes.as_ref())?;
            if crate::clvm::assemble::handle_int(token.bytes.as_ref()).is_none() {
                if let SExp::Atom(atom) = value {
                    value = self.byte_atom(atom.as_ref().to_vec());
                }
            }
            if value == NULL_SEXP
                && (self.compiler_version.load(Ordering::Relaxed) == 0
                    || (self.compiler_version.load(Ordering::Relaxed) != 21
                        && self.flags & NESTED_ASSIGN == 0))
            {
                Ok(NULL_SEXP)
            } else {
                Ok(QUOTE_SEXP.clone().cons(value))
            }
        }
    }

    fn get_constant(&'a self, token: Token<'a>) -> Result<SExp<'a>, Error> {
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
            let names = self.table_names.read();
            let index = names
                .iter()
                .position(|name| name == &token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Constant not found"))?;
            return Ok(self.get_path(&get_function_pointer(index, 0, names.len(), true)?));
        }
        let (index, _) = self
            .constants
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.name.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
        if self.flags & INLINE_CONSTS == INLINE_CONSTS
            || self.compiler_version.load(Ordering::Relaxed) >= 21
        {
            self.constants
                .read()
                .iter()
                .find(|v| v.name.bytes == token.bytes)
                .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))
                .map(|v| {
                    let mut value = parse_value(v.value[0].bytes.as_ref())?;
                    if crate::clvm::assemble::handle_int(v.value[0].bytes.as_ref()).is_none() {
                        if let SExp::Atom(atom) = value {
                            value = self.byte_atom(atom.as_ref().to_vec());
                        }
                    }
                    Ok(QUOTE_SEXP.clone().cons(value))
                })?
        } else {
            let const_pointer = get_const_pointer(index)?;
            Ok(self.get_path(&const_pointer))
        }
    }

    fn get_arg(&'a self, token: Token<'a>) -> Result<SExp<'a>, Error> {
        let (index, _) = self
            .argument_names
            .read()
            .iter()
            .enumerate()
            .find(|v| v.1.bytes == token.bytes)
            .ok_or(Error::new(ErrorKind::InvalidData, "Argument not found"))?;
        let arg_pointer = if self.functions.read().is_empty() && self.table_names.read().is_empty()
        {
            get_arg_pointer(index)?
        } else {
            get_arg_pointer(index + 1)?
        };
        Ok(self.get_path(&arg_pointer))
    }
}
