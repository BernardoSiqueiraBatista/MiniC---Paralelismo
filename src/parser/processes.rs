//! Process (actor) declaration parser for MiniC.
//!
//! # Overview
//!
//! Exposes one public function:
//!
//! * [`process_decl`] — parses `process name(Type param, …) { … }` into a
//!   [`ProcessDecl`].
//!
//! # Design Decisions
//!
//! ## No return type, block-only body
//!
//! A process is not called — it is started with `spawn` and communicates
//! only through messages, so it has no return type. Unlike `fun_decl`, whose
//! body is any statement, a process body must be a block: this keeps the
//! syntax uniform with `receive` clauses and leaves room for per-process
//! scoping in the semantic phase.

use crate::ir::ast::{ProcessDecl, UncheckedProcessDecl};
use crate::parser::functions::param_list;
use crate::parser::identifiers::{identifier, keyword};
use crate::parser::statements::block_statement;
use nom::{
    character::complete::{multispace0, multispace1},
    sequence::preceded,
    IResult,
};

/// Parse a process declaration: `process name ( params ) block`.
pub fn process_decl(input: &str) -> IResult<&str, UncheckedProcessDecl> {
    let (rest, _) = preceded(multispace0, keyword("process"))(input)?;
    let (rest, name) = preceded(multispace1, identifier)(rest)?;
    let (rest, params) = param_list(rest)?;
    let (rest, body) = preceded(multispace0, block_statement)(rest)?;
    Ok((
        rest,
        ProcessDecl {
            name: name.to_string(),
            params,
            body: Box::new(body),
        },
    ))
}
