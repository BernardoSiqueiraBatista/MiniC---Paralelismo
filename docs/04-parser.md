# 4 — The Parser

This document explains how the MiniC parser turns source text into an AST.
We start with a concrete example, then introduce the tools the parser uses.

---

## A Worked Example: Parsing `1 + 2`

Consider the expression `1 + 2`. The parser needs to produce the tree:

```
Add
├── Literal(1)
└── Literal(2)
```

Here is roughly what happens, step by step:

1. The parser calls the `expression` function with input `"1 + 2"`.
2. `expression` calls `additive`, which calls `multiplicative` to get the
   left operand.
3. `multiplicative` calls `unary`, which calls `primary`, which calls
   `atom`.
4. `atom` tries to match a literal — it sees `"1"` and returns
   `Literal(Int(1))`. The remaining input is `" + 2"`.
5. Back in `additive`: remaining input starts with `" + "`, so it matches
   the `+` operator and calls `multiplicative` again for the right side.
6. `multiplicative` → `unary` → `primary` → `atom` sees `"2"` and returns
   `Literal(Int(2))`. Remaining input is `""`.
7. `additive` builds `Expr::Add(Literal(1), Literal(2))` and returns it.

The same recursive logic handles arbitrarily complex expressions like
`a * (b + c) - sqrt(d)`.

---

## What is a Parser Combinator?

MiniC uses a library called **nom** to build the parser. `nom` provides
*combinators* — small functions that each recognise a tiny piece of syntax
and can be **composed** (combined) into parsers for larger constructs.

Every parser in `nom` follows the same contract:
- **Input:** a string slice `&str`
- **Output:** `IResult<&str, T>` — either `Ok((remaining, value))` on
  success, or `Err(...)` on failure

`remaining` is the part of the input that was not yet consumed. This is how
parsers chain: one parser hands its leftover input to the next.

---

## Key `nom` Combinators Used in MiniC

| Combinator | What it does | Example use |
|------------|-------------|-------------|
| `tag("x")` | Match the exact string `"x"` | `tag("if")` to match the keyword `if` |
| `char('x')` | Match the exact character `'x'` | `char('(')` for open parenthesis |
| `digit1` | Match one or more decimal digits | Integer literals |
| `alt((a, b, c))` | Try `a`; if it fails, try `b`; then `c` | Choice between statement forms |
| `tuple((a, b, c))` | Parse `a`, then `b`, then `c`; return all three | Parse keyword + name + body |
| `preceded(a, b)` | Parse `a` (discard), then parse `b` (keep) | Skip whitespace before a token |
| `delimited(a, b, c)` | Parse `a`, then `b`, then `c`; return only `b` | `(` expression `)` |
| `map(p, f)` | Parse with `p`, transform the result with function `f` | Build an AST node from raw text |
| `opt(p)` | Try `p`; return `Some(result)` or `None` | Optional `else` branch |
| `verify(p, pred)` | Parse with `p`; fail if `pred(result)` is false | Reject reserved words as identifiers |
| `many0(p)` | Apply `p` zero or more times; collect results | Parse all function declarations |

---

## Sub-module Decomposition

The parser is split into six files, each responsible for one grammatical
category. The dependencies flow from simple to complex:

```
program.rs        ← top-level entry point
  └── functions.rs    ← function declarations
        └── statements.rs   ← statements (if, while, block, …)
              └── expressions.rs  ← arithmetic, comparison, …
                    ├── literals.rs     ← numbers, strings, booleans
                    └── identifiers.rs  ← variable names
```

This mirrors how grammars are traditionally presented and makes it easy to
find the rule for any construct.

---

## Operator Precedence

The expression `1 + 2 * 3` must parse as `1 + (2 * 3)`, not `(1 + 2) * 3`.
This requires that `*` "binds tighter" than `+`.

MiniC encodes precedence through the **call chain**: each function calls the
next-higher-precedence function to get its operands. Higher in the chain =
tighter binding.

```
expression
    └── logical_or          (lowest precedence: or)
          └── logical_and   (and)
                └── logical_not   (!)
                      └── relational   (==  !=  <  <=  >  >=)
                            └── additive     (+  -)
                                  └── multiplicative   (*  /)
                                        └── unary      (unary -)
                                              └── primary   (atoms + indexing)
                                                    └── atom
```

When `additive` needs its right operand, it calls `multiplicative`. So `*`
always groups before `+` — naturally, without any precedence table.

**Why does this work?** Consider `1 + 2 * 3`:
1. `additive` reads `1` (via `multiplicative`).
2. It sees `+` and calls `multiplicative` again for the right side.
3. `multiplicative` reads `2 * 3` and returns `Mul(2, 3)` as a single unit.
4. `additive` builds `Add(1, Mul(2, 3))`. ✓

---

## Left-Associativity

For `1 - 2 - 3`, we want `(1 - 2) - 3`, not `1 - (2 - 3)`. This is called
*left-associativity*.

Each level that has left-associative operators uses an **accumulator loop**
instead of recursion:

```rust
fn additive(input) {
    let (rest, mut acc) = multiplicative(input)?;   // left-most operand
    loop {
        if let Ok((rest, right)) = parse("+" then multiplicative) {
            acc = Add(acc, right);   // fold left: (acc + right) becomes new acc
        } else if let Ok((rest, right)) = parse("-" then multiplicative) {
            acc = Sub(acc, right);
        } else {
            break;
        }
    }
    return acc;
}
```

For `1 - 2 - 3`:
- Start: `acc = 1`
- Loop 1: see `-2`, `acc = Sub(1, 2)`
- Loop 2: see `-3`, `acc = Sub(Sub(1, 2), 3)`
- Loop 3: no more `-` or `+`, stop.

Result: `(1 - 2) - 3` ✓

---

## Key Design Decisions

**No separate lexer.** Many compilers have two separate phases: a *lexer*
that turns characters into tokens (e.g., `"42"` → `Token::Int(42)`), then a
*parser* that processes tokens. MiniC skips the lexer and lets `nom` work
directly on characters. This is simpler for a small language and keeps the
code in one place.

**The parser produces an untyped AST.** Every node in the output carries
`ty: ()` — an empty placeholder. Type information is not the parser's
responsibility; it is computed in the separate type-checking stage. Keeping
parsing and type checking apart makes each phase shorter and independently
testable.

---

## Actor extension

MiniC is being extended with **concurrency based on the actor model**:
programs are made of isolated *processes* that share no memory and interact
only by exchanging messages. This first milestone covers **syntax, AST and
parser only**; the type checker and interpreter reject the new constructs
with an explicit `"... is not yet supported"` error (semantics and isolation
come in milestone 2).

### Grammar

```
program      := (fun_decl | process_decl)*          // any order
process_decl := 'process' ident '(' params ')' block // no return type; body must be a block
expr        += 'spawn' ident '(' args ')'           // evaluates to a pid
             | 'self'                               // pid of the current process
stmt        += 'send' expr '!' ident '(' args ')' ';'   // asynchronous
             | 'receive' '{' clause+ '}'            // blocking; no ';'
clause       := ident '(' params ')' '=>' block
type        += 'pid' | 'pid[]' | 'pid[][]'
```

New reserved words: `process`, `spawn`, `send`, `receive`, `self`, `pid`.

Example (`tests/fixtures/actor_counter.minic`):

```c
process counter(int start) {
    int n = start;
    bool running = true;
    while running {
        receive {
            inc(int k) => { n = n + k; }
            get(pid from) => { send from ! value(n); }
            stop() => { running = false; }
        }
    }
}

void main() {
    pid c = spawn counter(0);
    send c ! inc(5);
    send c ! get(self);
    receive {
        value(int v) => { print(v); }
    }
}
```

### AST (`src/ir/ast.rs`)

| Construct                 | Node                                                  |
|---------------------------|-------------------------------------------------------|
| `pid` type                | `Type::Pid`                                           |
| `spawn p(args)`           | `Expr::Spawn { process, args }`                       |
| `self`                    | `Expr::SelfPid`                                       |
| `tag(args)` in `send`     | `Message { tag, args }`                               |
| `send e ! m(args);`       | `Statement::Send { target, message }`                 |
| `receive { ... }`         | `Statement::Receive { clauses }`                      |
| `tag(params) => { ... }`  | `ReceiveClause { tag, params, body }`                 |
| `process p(params) {...}` | `ProcessDecl { name, params, body }`                  |
| whole program             | `Program { functions, processes }`                    |

### Parser modules

```
program.rs        ← many0(process_decl | fun_decl)
  ├── processes.rs    ← process_decl (new)
  └── functions.rs    ← fun_decl, type_name (+ pid), param_list (now shared)
        └── statements.rs   ← + send_statement, receive_statement; block_statement now public
              └── expressions.rs  ← + spawn_expr, self_expr (atoms)
                    └── identifiers.rs  ← + keyword(), actor reserved words
```

### Design decisions

**Keywords match whole words.** `keyword(kw)` is `tag(kw)` followed by
`not(identifier-char)`. Without it, `spawner(1)` would be read as
`spawn er(1)` and `selfish` as `self` + garbage. The new keywords are also
added to `RESERVED`, so they cannot be used as identifiers, while prefixes
such as `spawner`, `selfish`, `sender` remain valid names.

**`send`'s `!` vs. `!=`.** The target of `send` is a full expression. The
expression parser never consumes a lone infix `!` (only `!=`), so it stops
right before the separator; the separator itself is parsed as `!` *not
followed by* `=`. Hence `send a != b ! m();` sends `m()` to `a != b`, and
`send p != m();` is a syntax error rather than being silently split.

**Messages are tagged tuples.** A message is `tag(args)`, reusing the
call-argument syntax (`parse_call`). A `receive` clause mirrors it with a
parameter list, reusing `param_list` from `functions.rs` — the same rule
used by functions and processes — so pattern matching is by tag, with typed
bindings for the payload.

**Processes have no return type and a block body.** A process is started
with `spawn`, never called, so a return value would have nowhere to go.
Requiring a block (unlike `fun_decl`, which accepts any statement) keeps it
uniform with `receive` clauses, whose bodies are also blocks.

**`receive` needs at least one clause and no `;`.** `many1` rejects
`receive { }`, which would block forever. Like `if` and `while`, `receive`
ends in `}` and needs no terminator.

**Separate `processes` list.** `Program` keeps functions and processes in
two vectors (each in source order) instead of a mixed list. Existing code
that iterates `program.functions` keeps working unchanged, and later phases
can register process names in their own namespace.

**Later phases fail explicitly.** The type checker returns
`TypeError("process declarations are not yet supported")` (and similar for
`spawn`, `self`, `send`, `receive`); the interpreter does the same with
`RuntimeError`. The TAC code generator already ends its matches with a
catch-all arm, so it still compiles and is unaffected.

---

**What to read next →** [05-type-checker.md](05-type-checker.md)
