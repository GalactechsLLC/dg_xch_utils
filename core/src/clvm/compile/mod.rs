mod classic;
pub mod conditions;
mod optimize;
pub mod tokenizer;
pub mod utils;

use crate::clvm::compile::conditions::{
    parse_assign_pattern, parse_constant, parse_function, parse_include, read_form, read_include,
};
use crate::clvm::compile::tokenizer::{Token, TokenType, Tokenizer};
use crate::clvm::compile::utils::{
    concat_args, get_arg_pointer, get_const_pointer, get_function_pointer, get_program_size,
    parse_value, select_path,
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
        let mut stream = tokens[2..tokens.len() - 1].to_vec().into_iter();
        let mut forms = vec![];
        while !stream.as_slice().is_empty() {
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
                let mut stream = pattern[1..].to_vec().into_iter();
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
            let mut stream = bindings[1..bindings.len() - 1].to_vec().into_iter();
            while !stream.as_slice().is_empty() {
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
                let mut pair = binding[1..binding.len() - 1].to_vec().into_iter();
                forms.push(read_form(&mut pair)?);
                forms.push(read_form(&mut pair)?);
                if !pair.as_slice().is_empty() {
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
        for pair in forms.chunks_exact(2) {
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

    fn prepare_assigns(&'a self) -> Result<(), Error> {
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

    fn optimize_assigns(
        &'a self,
        functions: &[Function<'a>],
        mut inline_names: HashSet<Cow<'a, [u8]>>,
    ) -> Result<(), Error> {
        let dependencies: Vec<Vec<usize>> = functions
            .iter()
            .map(|function| {
                functions
                    .iter()
                    .enumerate()
                    .filter(|(_, other)| {
                        function
                            .function_body
                            .iter()
                            .any(|token| token.bytes == other.name.bytes)
                    })
                    .map(|(index, _)| index)
                    .collect()
            })
            .collect();
        let mut groups = vec![];
        for (index, function) in functions.iter().enumerate() {
            if !dependencies[index].is_empty()
                || (!function.generated && !inline_names.contains(&function.name.bytes))
            {
                continue;
            }
            let mut roots = vec![];
            let mut visited = HashSet::new();
            let mut pending = vec![index];
            while let Some(index) = pending.pop() {
                if !visited.insert(index) {
                    continue;
                }
                let function = &functions[index];
                if !function.generated && !inline_names.contains(&function.name.bytes) {
                    roots.push(index);
                } else {
                    pending.extend(
                        dependencies
                            .iter()
                            .enumerate()
                            .filter(|(_, deps)| deps.contains(&index))
                            .map(|(index, _)| index),
                    );
                }
            }
            if roots.is_empty() {
                roots.push(index);
            }
            roots.sort_by(|a, b| functions[*a].name.bytes.cmp(&functions[*b].name.bytes));
            roots.dedup();
            groups.push(roots);
        }
        groups.sort_by_key(|group| {
            group
                .iter()
                .map(|index| functions[*index].name.bytes.clone())
                .collect::<Vec<_>>()
        });
        groups.dedup();
        let groups: Vec<Vec<usize>> = groups
            .into_iter()
            .map(|roots| {
                let mut reached = HashSet::new();
                let mut pending: Vec<_> = roots
                    .iter()
                    .flat_map(|index| dependencies[*index].clone())
                    .collect();
                while let Some(index) = pending.pop() {
                    if reached.insert(index) {
                        pending.extend_from_slice(&dependencies[index]);
                    }
                }
                if reached.is_empty() {
                    reached.extend(roots);
                }
                let mut candidates: Vec<_> = reached
                    .into_iter()
                    .filter(|index| functions[*index].generated)
                    .collect();
                candidates.sort_by(|a, b| functions[*a].name.bytes.cmp(&functions[*b].name.bytes));
                candidates
            })
            .collect();
        let set_functions = |names: &HashSet<Cow<'a, [u8]>>| {
            let (inline, regular): (Vec<_>, Vec<_>) = functions
                .iter()
                .cloned()
                .partition(|f| names.contains(&f.name.bytes));
            *self.table_names.write() = self
                .declaration_order
                .read()
                .iter()
                .filter(|name| {
                    regular.iter().any(|f| &f.name.bytes == *name)
                        || self
                            .embedded_files
                            .read()
                            .iter()
                            .any(|(token, _)| &token.bytes == *name)
                })
                .cloned()
                .collect();
            *self.functions.write() = regular;
            *self.inline_functions.write() = inline;
        };
        if self.compiler_version.load(Ordering::Relaxed) == 21 {
            // CL21 keeps let helpers inline; later dialects select them by program size.
            inline_names.extend(
                functions
                    .iter()
                    .filter(|f| f.generated)
                    .map(|f| f.name.bytes.clone()),
            );
            set_functions(&inline_names);
            return Ok(());
        }
        set_functions(&inline_names);
        let program_size = || -> Result<u64, Error> {
            let program = self.process()?;
            Ok(
                if self.opt_level == OPT_SIZE || self.flags & COMPAT_CHIA == 0 {
                    program.serialized()?.as_ref().len() as u64
                } else {
                    get_program_size(program.sexp(), &self.byte_atoms.lock())
                },
            )
        };
        let mut size = program_size()?;
        for group in groups {
            loop {
                let previous_size = size;
                for index in &group {
                    let function = &functions[*index];
                    let was_inline = inline_names.remove(&function.name.bytes);
                    if !was_inline {
                        inline_names.insert(function.name.bytes.clone());
                    }
                    set_functions(&inline_names);
                    let candidate_size = program_size()?;
                    if candidate_size < size {
                        size = candidate_size;
                    } else {
                        if was_inline {
                            inline_names.insert(function.name.bytes.clone());
                        } else {
                            inline_names.remove(&function.name.bytes);
                        }
                        set_functions(&inline_names);
                    }
                }
                if size == previous_size {
                    break;
                }
            }
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
        }
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
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
            && entries.iter().skip(1).all(|value| {
                *value == NULL_SEXP
                    || matches!(value, SExp::Pair(pair) if pair.first() == &QUOTE_SEXP)
            })
        {
            let program = SExp::from(entries.clone());
            if let Ok((_, value)) =
                crate::clvm::runtime::ClvmRuntime::new(10_000_000, 0).run(&program, &NULL_SEXP)
            {
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

    fn process_quasiquote(
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
            && args.iter().all(|value| {
                *value == NULL_SEXP
                    || matches!(value, SExp::Pair(pair) if pair.first() == &QUOTE_SEXP)
            })
            && matches!(operator.bytes.as_ref(), b"c" | b"sha256")
        {
            let mut entries = vec![B_KEYWORD_TO_SEXP[operator.bytes.as_ref()].clone()];
            entries.extend(args.clone());
            if let Ok((_, value)) = crate::clvm::runtime::ClvmRuntime::new(10_000_000, 0)
                .run(&SExp::from(entries), &NULL_SEXP)
            {
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
                let operator = B_KEYWORD_TO_SEXP
                    .get(operator.bytes.as_ref())
                    .cloned()
                    .unwrap_or(self.process_atom(
                        operator,
                        function_args,
                        mapped_args,
                        env_depth,
                    )?);
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

    fn get_function(
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

    fn get_program_args_sexp(&'a self) -> Result<SExp<'a>, Error> {
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
        if !entries.is_empty() {
            entries = vec![Self::get_function_tree(&entries)];
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

    fn get_function_body(&'a self, function: Function<'a>) -> Result<SExp<'a>, Error> {
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
            self.optimize_old_expression(value)
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

    fn curry_value(&'a self, program: SExp<'a>, argument: SExp<'a>) -> Result<SExp<'a>, Error> {
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

    fn optimize_old_expression(&'a self, value: SExp<'a>) -> Result<SExp<'a>, Error> {
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
            entries.push(self.optimize_old_expression(arg.to_owned())?);
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
        if matches!(&entries[0], SExp::Atom(atom) if matches!(atom.as_ref(), [4] | [11]))
            && entries.iter().skip(1).all(|arg| {
                *arg == NULL_SEXP || matches!(arg, SExp::Pair(pair) if pair.first() == &QUOTE_SEXP)
            })
        {
            if let Ok((_, result)) = crate::clvm::runtime::ClvmRuntime::new(10_000_000, 0)
                .run(&SExp::from(entries.clone()), &NULL_SEXP)
            {
                return Ok(QUOTE_SEXP.clone().cons(result));
            }
        }
        self.create_pair_sexp(entries)
    }

    fn select_path(
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

    fn get_inline_function(
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
        if func.argument_pattern.len() == 1
            && func
                .function_body
                .iter()
                .any(|token| token.bytes == func.argument_names[0].bytes)
        {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "Classic inline whole-argument references are not supported yet",
            ));
        }
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
                    self.select_path(args[index].clone(), path >> 1usize)
                })
                .collect::<Result<Vec<_>, _>>()?;
        }
        if func.has_rest_arg {
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
        result
    }

    fn can_inline_function(&'a self, function: &Function) -> bool {
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

    fn get_path(&self, path: &num_bigint::BigInt) -> SExp<'static> {
        if self.compiler_version.load(Ordering::Relaxed) == 0 {
            SExp::from(path.to_bytes_be().1)
        } else {
            SExp::from(path)
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
    fn ensure_token(&'a self, t_type: TokenType) -> Result<Token<'a>, Error> {
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
    fn parse_conditions(&'a self) -> Result<(), Error> {
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
            b"defun" => {
                let function =
                    parse_function(&mut conditions_queue, self.flags & COMPAT_CHIA != 0)?;
                self.reference_names.lock().record_function(&function)?;
                self.declaration_order
                    .write()
                    .push(function.name.bytes.clone());
                self.functions.write().push(function);
            }
            b"defun-inline" => {
                let function =
                    parse_function(&mut conditions_queue, self.flags & COMPAT_CHIA != 0)?;
                self.reference_names.lock().record_function(&function)?;
                self.declaration_order
                    .write()
                    .push(function.name.bytes.clone());
                self.inline_functions.write().push(function);
            }
            b"include" => {
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
                    return Ok(());
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
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    format!("Unexpected Expression: {operator:?}"),
                ));
            }
        }
        Ok(())
    }
}
