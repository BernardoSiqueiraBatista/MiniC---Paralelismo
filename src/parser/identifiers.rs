//! Identifier parser for MiniC.
//!
//! # Overview
//!
//! Exposes two public functions:
//!
//! * [`identifier`] — parses a valid MiniC identifier: starts with a letter
//!   or underscore, followed by letters, digits, or underscores. Rejects
//!   reserved words (`true`, `false`, `int`, `float`, `bool`, `str`, `void`,
//!   `return`, and the actor keywords `process`, `spawn`, `send`, `receive`,
//!   `self`, `pid`).
//! * [`keyword`] — builds a parser that matches a keyword as a *whole word*,
//!   so `spawner` or `selfish` are not mistaken for `spawn` / `self`.
//!
//! # Design Decisions
//!
//! ## Rejecting reserved words with `verify`
//!
//! `nom`'s `verify` combinator runs a predicate on the parsed result and
//! fails the parser if the predicate returns `false`. This is the cleanest
//! way to layer a keyword-exclusion rule on top of a general pattern match:
//! the character-class rule is written once, and the reserved-word check is
//! added as a separate, readable step. The alternative — listing every
//! keyword as a negative lookahead — would be more fragile to maintain.

use nom::{
    bytes::complete::{tag, take_while, take_while1},
    combinator::{not, recognize, verify},
    sequence::{pair, terminated},
    IResult,
};

/// Reserved words: boolean literals, type names, and actor keywords.
const RESERVED: &[&str] = &[
    "true", "false", "int", "float", "bool", "str", "void", "return",
    "process", "spawn", "send", "receive", "self", "pid",
];

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Parse the keyword `kw` as a whole word: it must not be followed by an
/// identifier character (so `spawn` matches in `spawn f()` but not in `spawner`).
pub fn keyword<'a>(kw: &'static str) -> impl FnMut(&'a str) -> IResult<&'a str, &'a str> {
    terminated(tag(kw), not(take_while1(is_ident_char)))
}

/// Parse an identifier (variable name).
/// Must start with letter or underscore; subsequent chars may be letter, digit, or underscore.
/// Rejects reserved words (type names, literals, `return` and the actor keywords).
pub fn identifier(input: &str) -> IResult<&str, &str> {
    let id_parser = recognize(pair(
        take_while1(|c: char| c.is_alphabetic() || c == '_'),
        take_while(is_ident_char),
    ));
    verify(id_parser, |s: &str| !RESERVED.contains(&s))(input)
}
