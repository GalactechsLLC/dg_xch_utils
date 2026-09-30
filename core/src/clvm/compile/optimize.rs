use crate::clvm::compile::Function;
use crate::clvm::compile::conditions::read_form;
use crate::clvm::compile::tokenizer::{Token, TokenType};
use crate::clvm::compile::utils::parse_value;
use crate::clvm::sexp::SExp;
use crate::constants::NULL_SEXP;
use crate::traits::SizedBytes;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::Error;
use std::ops::Range;

// Only ordering metadata is reproduced here; the standard macros themselves
// are implemented directly by the compiler. chialisp 0.4.5 with CL26 reserves
// 48 initial symbols
// and 14 when renaming its standard helpers.
#[derive(Debug)]
pub struct ReferenceNames {
    next: usize,
    list_expanded: bool,
    pub arguments: HashMap<Vec<u8>, HashMap<Vec<u8>, Vec<u8>>>,
}

impl Default for ReferenceNames {
    fn default() -> Self {
        Self {
            next: 48,
            list_expanded: false,
            arguments: HashMap::new(),
        }
    }
}

impl ReferenceNames {
    pub fn record_body(&mut self, tokens: &[Token<'_>]) -> Result<usize, Error> {
        let tokens = read_form(&mut tokens.to_vec().into_iter())?;
        let mut expressions = vec![];
        read_expression(&tokens, 0..tokens.len(), None, &mut expressions)?;
        let mut count = 0;
        for expression in &expressions {
            if expression.range.len() < 3
                || tokens[expression.range.start].t_type != TokenType::StartCons
            {
                continue;
            }
            match tokens[expression.range.start + 1].bytes.as_ref() {
                b"list" if !self.list_expanded => {
                    self.next += 3;
                    self.list_expanded = true;
                }
                b"assign" => {
                    for child in expression
                        .children
                        .iter()
                        .take(expression.children.len().saturating_sub(1))
                        .step_by(2)
                    {
                        count += tokens[expressions[*child].range.clone()]
                            .iter()
                            .filter(|token| token.t_type == TokenType::Expression)
                            .count();
                    }
                }
                _ => {}
            }
        }
        Ok(count)
    }

    pub fn record_function(&mut self, function: &Function<'_>) -> Result<(), Error> {
        self.next += function.argument_names.len() + self.record_body(&function.function_body)?;
        Ok(())
    }

    pub fn finish(&mut self, functions: &[Function<'_>]) -> Result<(), Error> {
        self.next += 14;
        for function in functions {
            let mut names = HashMap::new();
            for argument in &function.argument_names {
                self.next += 1;
                let mut name = argument.bytes.to_vec();
                name.extend(format!("_$_{}", self.next).as_bytes());
                names.insert(argument.bytes.to_vec(), name);
            }
            self.next += self.record_body(&function.function_body)?;
            self.arguments.insert(function.name.bytes.to_vec(), names);
        }
        Ok(())
    }
}

struct Expression {
    range: Range<usize>,
    children: Vec<usize>,
    parent: Option<usize>,
}

fn read_expression(
    tokens: &[Token<'_>],
    range: Range<usize>,
    parent: Option<usize>,
    expressions: &mut Vec<Expression>,
) -> Result<usize, Error> {
    let index = expressions.len();
    expressions.push(Expression {
        range: range.clone(),
        children: vec![],
        parent,
    });
    if tokens[range.start].t_type != TokenType::StartCons || range.len() < 3 {
        return Ok(index);
    }
    if matches!(tokens[range.start + 1].bytes.as_ref(), b"q" | b"quote") {
        return Ok(index);
    }
    let mut stream = tokens[range.start + 2..range.end - 1].to_vec().into_iter();
    let mut position = range.start + 2;
    while !stream.as_slice().is_empty() {
        let form = read_form(&mut stream)?;
        let end = position + form.len();
        let child = read_expression(tokens, position..end, Some(index), expressions)?;
        expressions[index].children.push(child);
        position = end;
    }
    Ok(index)
}

// Keep repeated expressions in the source preprocessing layer. The generated
// parallel bindings use the same environment and inlining machinery as assign.
pub fn share_expressions<'a>(
    tokens: Vec<Token<'a>>,
    names: &HashMap<Vec<u8>, Vec<u8>>,
) -> Result<Vec<Token<'a>>, Error> {
    let tokens = read_form(&mut tokens.into_iter())?;
    let mut expressions = vec![];
    read_expression(&tokens, 0..tokens.len(), None, &mut expressions)?;
    let operator = |index: usize| {
        let range = &expressions[index].range;
        if range.len() > 2 && tokens[range.start].t_type == TokenType::StartCons {
            tokens[range.start + 1].bytes.as_ref()
        } else {
            b""
        }
    };
    let mut bound = HashSet::new();
    let mut patterns = HashSet::new();
    for (index, expression) in expressions.iter().enumerate() {
        if operator(index) == b"assign" {
            for child in expression
                .children
                .iter()
                .take(expression.children.len().saturating_sub(1))
                .step_by(2)
            {
                let range = expressions[*child].range.clone();
                patterns.extend(range.clone());
                bound.extend(
                    tokens[range]
                        .iter()
                        .filter(|token| token.t_type == TokenType::Expression)
                        .map(|token| token.bytes.clone()),
                );
            }
        }
    }
    let mut groups: HashMap<Vec<Cow<'a, [u8]>>, Vec<usize>> = HashMap::new();
    for (index, expression) in expressions.iter().enumerate().skip(1) {
        if expression.children.is_empty()
            || patterns.contains(&expression.range.start)
            || matches!(operator(index), b"assign" | b"q" | b"quote")
            || tokens[expression.range.clone()]
                .iter()
                .any(|token| bound.contains(&token.bytes))
        {
            continue;
        }
        groups
            .entry(
                tokens[expression.range.clone()]
                    .iter()
                    .map(|token| token.bytes.clone())
                    .collect(),
            )
            .or_default()
            .push(index);
    }
    let groups: Vec<_> = groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect();
    let mut candidates = vec![];
    for (index, group) in groups.iter().enumerate() {
        let hosts: HashSet<_> = groups
            .iter()
            .enumerate()
            .filter(|(other, host)| {
                *other != index
                    && group.iter().any(|child| {
                        host.iter().any(|parent| {
                            expressions[*parent].range.start < expressions[*child].range.start
                                && expressions[*parent].range.end >= expressions[*child].range.end
                        })
                    })
            })
            .map(|(index, _)| index)
            .collect();
        if hosts.len() == 1 {
            continue;
        }
        let mut parent = expressions[group[0]].parent;
        let mut covered = true;
        while let Some(index) = parent {
            let expression = &expressions[index];
            if operator(index) == b"if" && expression.children.len() == 3 {
                let used: Vec<_> = expression
                    .children
                    .iter()
                    .map(|child| {
                        let range = &expressions[*child].range;
                        group.iter().any(|instance| {
                            range.start <= expressions[*instance].range.start
                                && range.end >= expressions[*instance].range.end
                        })
                    })
                    .collect();
                if !used[0] && !(used[1] && used[2]) {
                    covered = false;
                    break;
                }
            }
            parent = expression.parent;
        }
        if covered {
            candidates.push(group.clone());
        }
    }
    // Reference ordering uses hashes of the uniquely named source expressions.
    let mut hashes = vec![NULL_SEXP; expressions.len()];
    for (index, expression) in expressions.iter().enumerate().rev() {
        hashes[index] = if expression.children.is_empty() {
            let token = &tokens[expression.range.start];
            if let Some(name) = names.get(token.bytes.as_ref()) {
                SExp::from(name.clone())
            } else if token.t_type == TokenType::Expression {
                parse_value(&token.bytes)?
            } else {
                NULL_SEXP
            }
        } else {
            let mut value = NULL_SEXP;
            for child in expression.children.iter().rev() {
                value = hashes[*child].clone().cons(value);
            }
            SExp::from(operator(index).to_vec()).cons(value)
        };
    }
    candidates.sort_by_key(|group| {
        (
            expressions[group[0]].range.len(),
            hashes[group[0]].tree_hash().bytes(),
        )
    });
    let mut replacements: HashMap<usize, Token<'a>> = HashMap::new();
    let mut bindings: HashMap<usize, Vec<(Token<'a>, Vec<Token<'a>>)>> = HashMap::new();
    for (index, group) in candidates.iter().enumerate() {
        let first = &expressions[group[0]];
        let mut target = first.parent;
        while let Some(parent) = target {
            let range = &expressions[parent].range;
            if group.iter().all(|instance| {
                range.start <= expressions[*instance].range.start
                    && range.end >= expressions[*instance].range.end
            }) {
                break;
            }
            target = expressions[parent].parent;
        }
        let mut target = target.unwrap_or(0);
        while target != 0 {
            let parent = expressions[target].parent.unwrap();
            if operator(parent) == b"assign" && expressions[parent].children.last() == Some(&target)
            {
                break;
            }
            target = parent;
        }
        let name = Token {
            bytes: Cow::Owned(format!("\0shared_{index:08}").into_bytes()),
            index: tokens[first.range.start].index,
            t_type: TokenType::Expression,
        };
        let value = write_expression(
            group[0],
            &tokens,
            &expressions,
            &replacements,
            &HashMap::new(),
        );
        bindings
            .entry(target)
            .or_default()
            .push((name.clone(), value));
        for instance in group {
            replacements.insert(*instance, name.clone());
        }
    }
    Ok(write_expression(
        0,
        &tokens,
        &expressions,
        &replacements,
        &bindings,
    ))
}

fn write_expression<'a>(
    index: usize,
    tokens: &[Token<'a>],
    expressions: &[Expression],
    replacements: &HashMap<usize, Token<'a>>,
    bindings: &HashMap<usize, Vec<(Token<'a>, Vec<Token<'a>>)>>,
) -> Vec<Token<'a>> {
    if let Some(name) = replacements.get(&index) {
        return vec![name.clone()];
    }
    let expression = &expressions[index];
    let mut result = vec![];
    let mut position = expression.range.start;
    for child in &expression.children {
        result.extend_from_slice(&tokens[position..expressions[*child].range.start]);
        result.extend(write_expression(
            *child,
            tokens,
            expressions,
            replacements,
            bindings,
        ));
        position = expressions[*child].range.end;
    }
    result.extend_from_slice(&tokens[position..expression.range.end]);
    if let Some(bindings) = bindings.get(&index) {
        let token = &tokens[expression.range.start];
        let mut output = vec![
            Token {
                bytes: Cow::Borrowed(b"("),
                t_type: TokenType::StartCons,
                ..token.clone()
            },
            Token {
                bytes: Cow::Borrowed(b"\0let"),
                t_type: TokenType::Expression,
                ..token.clone()
            },
        ];
        for (name, value) in bindings {
            output.push(name.clone());
            output.extend(value.clone());
        }
        output.extend(result);
        output.push(Token {
            bytes: Cow::Borrowed(b")"),
            t_type: TokenType::EndCons,
            ..token.clone()
        });
        result = output;
    }
    result
}
