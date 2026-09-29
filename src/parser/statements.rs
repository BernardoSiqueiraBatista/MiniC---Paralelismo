//! Statement parsers for MiniC.
//!
//! # Overview
//!
//! Exposes two public functions:
//!
//! * [`statement`] — the top-level entry point; tries each statement form in
//!   order: block, `if`, `while`, `receive`, `send`, `return`, declaration,
//!   call-statement, assignment.
//! * [`block_statement`] — parses `{ stmt* }`; re-used by process
//!   declarations, whose body must be a block.
//! * [`assignment`] — parses `lvalue = expression ;`; exported separately
//!   because the test suite uses it directly.
//!
//! # Grammar
//!
//! ```text
//! statement  := block | if_stmt | while_stmt | receive | simple ';'
//! block      := '{' statement* '}'
//! if_stmt    := 'if' expr block ['else' block]
//! while_stmt := 'while' expr block
//! receive    := 'receive' '{' clause+ '}'
//! clause     := ident '(' params ')' '=>' block
//! simple     := return | send | decl | call | assign
//! send       := 'send' expr '!' ident '(' args ')'
//! ```
//!
//! Every simple statement is terminated by `;`.
//! Compound statements (`if`, `while`, `receive`, block) end with `}` and need no `;`.
//!
//! ## `send` target vs. the `!=` operator
//!
//! In `send target ! tag(args);` the target is a full expression. The
//! expression parser never consumes a lone `!` in infix position (only `!=`),
//! so it stops right before the message separator. The separator itself is
//! parsed as `!` *not followed by* `=`, so `send a != b ! m();` means
//! "send `m()` to the result of `a != b`" and never splits `!=`.
//!
//! # Design Decisions
//!
//! ## Declaration must be tried before assignment
//!
//! Both `int x = 0` (declaration) and `x = 0` (assignment) begin with an
//! identifier-like token, so the order of alternatives in [`statement`]
//! matters. Declaration is tried first because it starts with a type keyword
//! (`int`, `float`, …), which is unambiguous. If declaration fails, the
//! parser backtracks and tries assignment.
//!
//! ## `lvalue` handles nested array indexing on the left-hand side
//!
//! An assignment target can be a plain variable (`x = …`) or a nested array
//! element (`a[i][j] = …`). The private `lvalue` parser accumulates index
//! suffixes in a loop using the same pattern as the `primary` parser in
//! `expressions.rs`, producing a left-associative `Index` chain.

use crate::ir::ast::{
    Expr, ExprD, Message, ReceiveClause, Statement, StatementD, UncheckedExpr, UncheckedStmt,
};
use crate::parser::expressions::{expression, parse_call};
use crate::parser::functions::{param_list, type_name};
use crate::parser::identifiers::{identifier, keyword};
use nom::{
    branch::alt,
    bytes::complete::tag,
    character::complete::{char, multispace0},
    combinator::{map, not, opt},
    multi::{many0, many1},
    sequence::{delimited, preceded, terminated, tuple},
    IResult,
};

fn wrap(s: Statement<()>) -> UncheckedStmt {
    StatementD { stmt: s, ty: () }
}

/// Parse any statement: block | if | while | receive | send | return | decl | call | assignment.
pub fn statement(input: &str) -> IResult<&str, UncheckedStmt> {
    preceded(
        multispace0,
        alt((
            block_statement,
            if_statement,
            while_statement,
            receive_statement,
            send_statement,
            return_statement,
            decl_statement,
            call_statement,
            assignment,
        )),
    )(input)
}

/// Parse a return statement: `return [expr] ;`.
fn return_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, _) = preceded(multispace0, tag("return"))(input)?;
    let (rest, expr) = opt(preceded(multispace0, expression))(rest)?;
    let (rest, _) = preceded(multispace0, char(';'))(rest)?;
    Ok((rest, wrap(Statement::Return(expr.map(Box::new)))))
}

/// Parse a variable declaration: `Type ident = expr ;`. Must come before assignment.
fn decl_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    map(
        tuple((
            type_name,
            preceded(nom::character::complete::multispace1, identifier),
            preceded(multispace0, nom::bytes::complete::tag("=")),
            preceded(multispace0, expression),
            preceded(multispace0, char(';')),
        )),
        |(ty, name, _, init, _)| {
            wrap(Statement::Decl {
                name: name.to_string(),
                ty,
                init: Box::new(init),
            })
        },
    )(input)
}

/// Parse a block statement: `{ stmt* }`.
/// Each statement inside the block carries its own terminator (`;` or `}`).
pub fn block_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    map(
        delimited(
            preceded(multispace0, char('{')),
            many0(preceded(multispace0, statement)),
            preceded(multispace0, char('}')),
        ),
        |seq| wrap(Statement::Block { seq }),
    )(input)
}

/// Parse a function call as a statement: `identifier ( expr_list ) ;`.
fn call_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, (name, args)) = parse_call(input)?;
    let (rest, _) = preceded(multispace0, char(';'))(rest)?;
    Ok((rest, wrap(Statement::Call { name, args })))
}

/// Parse an asynchronous send: `send expr ! ident ( expr_list ) ;`.
/// The `!` separator must not be followed by `=` (that would be `!=`).
fn send_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, _) = preceded(multispace0, keyword("send"))(input)?;
    let (rest, target) = preceded(multispace0, expression)(rest)?;
    let (rest, _) = preceded(multispace0, terminated(char('!'), not(char('='))))(rest)?;
    let (rest, (tag, args)) = parse_call(rest)?;
    let (rest, _) = preceded(multispace0, char(';'))(rest)?;
    Ok((
        rest,
        wrap(Statement::Send {
            target: Box::new(target),
            message: Message { tag, args },
        }),
    ))
}

/// Parse a receive clause: `ident ( params ) => block`.
fn receive_clause(input: &str) -> IResult<&str, ReceiveClause<()>> {
    let (rest, tag_name) = preceded(multispace0, identifier)(input)?;
    let (rest, params) = param_list(rest)?;
    let (rest, _) = preceded(multispace0, tag("=>"))(rest)?;
    let (rest, body) = preceded(multispace0, block_statement)(rest)?;
    Ok((
        rest,
        ReceiveClause {
            tag: tag_name.to_string(),
            params,
            body: Box::new(body),
        },
    ))
}

/// Parse a blocking receive: `receive { clause+ }`. At least one clause is required.
fn receive_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, _) = preceded(multispace0, keyword("receive"))(input)?;
    let (rest, clauses) = delimited(
        preceded(multispace0, char('{')),
        many1(receive_clause),
        preceded(multispace0, char('}')),
    )(rest)?;
    Ok((rest, wrap(Statement::Receive { clauses })))
}

/// Parse an if statement: `if expr block ['else' block]`.
/// Both branches must be blocks — bare statements are not allowed.
fn if_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, _) = preceded(multispace0, tag("if"))(input)?;
    let (rest, cond) = preceded(multispace0, expression)(rest)?;
    let (rest, then_branch) = preceded(multispace0, block_statement)(rest)?;
    let (rest, else_branch) = opt(map(
        tuple((
            preceded(multispace0, tag("else")),
            preceded(multispace0, block_statement),
        )),
        |(_, stmt)| stmt,
    ))(rest)?;
    Ok((
        rest,
        wrap(Statement::If {
            cond: Box::new(cond),
            then_branch: Box::new(then_branch),
            else_branch: else_branch.map(Box::new),
        }),
    ))
}

/// Parse a while statement: `while expr block`.
/// The body must be a block — bare statements are not allowed.
fn while_statement(input: &str) -> IResult<&str, UncheckedStmt> {
    let (rest, _) = preceded(multispace0, tag("while"))(input)?;
    let (rest, cond) = preceded(multispace0, expression)(rest)?;
    let (rest, body) = preceded(multispace0, block_statement)(rest)?;
    Ok((
        rest,
        wrap(Statement::While {
            cond: Box::new(cond),
            body: Box::new(body),
        }),
    ))
}

/// Parse an lvalue: identifier followed by zero or more `[ expr ]` suffixes.
fn lvalue(input: &str) -> IResult<&str, UncheckedExpr> {
    let (mut rest, id) = preceded(multispace0, identifier)(input)?;
    let mut acc = ExprD {
        exp: Expr::Ident(id.to_string()),
        ty: (),
    };
    loop {
        let index_parse = delimited(
            preceded(multispace0, char('[')),
            preceded(multispace0, expression),
            preceded(multispace0, char(']')),
        )(rest);
        match index_parse {
            Ok((r, index)) => {
                acc = ExprD {
                    exp: Expr::Index {
                        base: Box::new(acc),
                        index: Box::new(index),
                    },
                    ty: (),
                };
                rest = r;
            }
            Err(_) => break,
        }
    }
    Ok((rest, acc))
}

/// Parse an assignment statement: `lvalue = expression ;`.
pub fn assignment(input: &str) -> IResult<&str, UncheckedStmt> {
    map(
        tuple((
            lvalue,
            preceded(multispace0, nom::bytes::complete::tag("=")),
            preceded(multispace0, expression),
            preceded(multispace0, char(';')),
        )),
        |(target, _, value, _)| {
            wrap(Statement::Assign {
                target: Box::new(target),
                value: Box::new(value),
            })
        },
    )(input)
}
