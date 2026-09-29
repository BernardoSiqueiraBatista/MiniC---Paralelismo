//! Top-level program parser for MiniC.
//!
//! # Overview
//!
//! Exposes one public function:
//!
//! * [`program`] — parses a complete MiniC program as a sequence of zero or
//!   more function and process declarations (in any order) and returns an
//!   [`UncheckedProgram`].
//!
//! A valid MiniC program contains **only** function and process declarations
//! at the top level — there are no top-level statements or variable
//! declarations. This constraint is enforced here by the grammar: `program`
//! is defined as `many0(fun_decl | process_decl)`, so any token that does not
//! start a declaration causes the parse to stop. The type checker then
//! verifies that a `main` function exists.
//!
//! # Design Decisions
//!
//! ## `many0` as the top-level combinator
//!
//! `nom`'s `many0` combinator repeatedly applies a parser until it fails,
//! collecting results in a `Vec`. Using it here means the program parser
//! naturally handles empty programs (zero functions) and programs with any
//! number of functions with no extra branching logic. The existence of
//! `main` is a semantic constraint checked in the next pipeline stage, not
//! a syntactic one enforced here.

use crate::ir::ast::{Program, UncheckedFunDecl, UncheckedProcessDecl, UncheckedProgram};
use crate::parser::functions::fun_decl;
use crate::parser::processes::process_decl;
use nom::{branch::alt, combinator::map, multi::many0, IResult};

enum TopLevel {
    Fun(UncheckedFunDecl),
    Process(UncheckedProcessDecl),
}

/// Parse a complete MiniC program: zero or more function and process
/// declarations, in any order. Each kind keeps its source order in `Program`.
/// Execution starts at the `main` function (validated by the type checker).
pub fn program(input: &str) -> IResult<&str, UncheckedProgram> {
    map(
        many0(alt((
            map(process_decl, TopLevel::Process),
            map(fun_decl, TopLevel::Fun),
        ))),
        |decls| {
            let mut functions = Vec::new();
            let mut processes = Vec::new();
            for d in decls {
                match d {
                    TopLevel::Fun(f) => functions.push(f),
                    TopLevel::Process(p) => processes.push(p),
                }
            }
            Program {
                functions,
                processes,
            }
        },
    )(input)
}
