//! Function declaration and type-name parsers for MiniC.
//!
//! # Overview
//!
//! Exposes three public functions:
//!
//! * [`fun_decl`] — parses a complete function declaration in C style:
//!   `ReturnType name(Type param, …) body`. The body is any single
//!   statement (typically a block `{ … }`).
//! * [`type_name`] — parses a MiniC type keyword (`int`, `float`, `bool`,
//!   `str`, `void`, or an array variant like `int[]`). Re-used by the
//!   statement parser for variable declarations.
//! * [`param_list`] — parses a parenthesised, comma-separated parameter list
//!   `(Type name, …)`. Shared by functions, processes and `receive` clauses.
//!
//! # Design Decisions
//!
//! ## 2D array types must be tried before 1D
//!
//! `nom`'s `alt` combinator tries alternatives left-to-right and stops at
//! the first match. Because `int[][]` starts with the same prefix as
//! `int[]`, the 2D variants must appear before the 1D variants in the
//! `alt` list, otherwise `int[][]` would be incorrectly parsed as `int[]`
//! followed by a leftover `[]`.

use crate::ir::ast::{FunDecl, Param, Type, UncheckedFunDecl};
use crate::parser::identifiers::identifier;
use crate::parser::statements::statement;
use nom::{
    branch::alt,
    bytes::complete::tag,
    character::complete::{multispace0, multispace1},
    combinator::map,
    multi::separated_list0,
    sequence::{delimited, preceded, tuple},
    IResult,
};

/// Parse a type name: int | float | bool | str | pid | void | T[] | T[][] (C-style lowercase).
pub fn type_name(input: &str) -> IResult<&str, Type> {
    preceded(
        multispace0,
        alt((
            // 2D arrays must be tried before 1D (longer prefix first)
            map(tag("int[][]"), |_| Type::Array(Box::new(Type::Array(Box::new(Type::Int))))),
            map(tag("float[][]"), |_| Type::Array(Box::new(Type::Array(Box::new(Type::Float))))),
            map(tag("bool[][]"), |_| Type::Array(Box::new(Type::Array(Box::new(Type::Bool))))),
            map(tag("str[][]"), |_| Type::Array(Box::new(Type::Array(Box::new(Type::Str))))),
            map(tag("pid[][]"), |_| Type::Array(Box::new(Type::Array(Box::new(Type::Pid))))),
            map(tag("int[]"), |_| Type::Array(Box::new(Type::Int))),
            map(tag("float[]"), |_| Type::Array(Box::new(Type::Float))),
            map(tag("bool[]"), |_| Type::Array(Box::new(Type::Bool))),
            map(tag("str[]"), |_| Type::Array(Box::new(Type::Str))),
            map(tag("pid[]"), |_| Type::Array(Box::new(Type::Pid))),
            map(tag("int"), |_| Type::Int),
            map(tag("float"), |_| Type::Float),
            map(tag("bool"), |_| Type::Bool),
            map(tag("str"), |_| Type::Str),
            map(tag("pid"), |_| Type::Pid),
            map(tag("void"), |_| Type::Unit),
        )),
    )(input)
}

/// Parse a typed parameter (C-style): `Type name`.
fn param(input: &str) -> IResult<&str, Param> {
    map(
        tuple((
            preceded(multispace0, type_name),
            preceded(multispace1, identifier),
        )),
        |(ty, name)| -> (String, Type) { (name.to_string(), ty) },
    )(input)
}

/// Parse a parameter list: `( Type name, ... )`.
pub fn param_list(input: &str) -> IResult<&str, Vec<Param>> {
    delimited(
        preceded(multispace0, tag("(")),
        separated_list0(preceded(multispace0, tag(",")), param),
        preceded(multispace0, tag(")")),
    )(input)
}

/// Parse a function declaration (C-style): `ReturnType name(Type name, ...) body`.
/// Example: `int add(int x, int y) { ... }` or `void main() x = 1`.
pub fn fun_decl(input: &str) -> IResult<&str, UncheckedFunDecl> {
    let (rest, return_type) = preceded(multispace0, type_name)(input)?;
    let (rest, name) = preceded(multispace1, identifier)(rest)?;
    let (rest, params) = param_list(rest)?;
    let (rest, body) = preceded(multispace0, statement)(rest)?;
    Ok((
        rest,
        FunDecl {
            name: name.to_string(),
            params,
            return_type,
            body: Box::new(body),
        },
    ))
}
