# 1 — The MiniC Language

This document describes MiniC from a **user** perspective: what you can write,
what the rules are, and what a complete program looks like. No knowledge of
the implementation is needed here.

---

## Types

MiniC has seven types:

| Type | Description | Example values |
|------|-------------|----------------|
| `int` | 64-bit integer | `0`, `42`, `-7` |
| `float` | 64-bit floating-point number | `3.14`, `-0.5`, `1.0` |
| `bool` | Boolean | `true`, `false` |
| `str` | Text string | `"hello"`, `"world"` |
| `pid` | Process identifier (actor address) | `self`, `spawn counter(0)` |
| `void` | No value; used only as a function return type | — |
| `T[]` | Array of elements of type `T` | `[1, 2, 3]`, `[true, false]` |

Arrays can be nested: `int[][]` is a 2D array of integers.

---

## Program Structure

A MiniC program is a list of **function declarations**. There are no top-level
statements. Execution always starts at `main`.

```c
void main() {
  print("hello, world")
}
```

Every function has a return type, a name, a (possibly empty) parameter list,
and a body — which is a single statement (often a block `{ … }`).

```c
int add(int x, int y) {
  return x + y
}
```

---

## Variables

Variables must be **declared and initialised** in one step using a type
annotation:

```c
int x = 42
float pi = 3.14159
bool flag = true
str name = "Alice"
int[] nums = [1, 2, 3]
```

There are no uninitialised variables in MiniC.

---

## Statements

### Variable declaration

```c
int count = 0
```

### Assignment

```c
count = count + 1
```

Array elements can be assigned by index:

```c
nums[0] = 99
matrix[i][j] = 0
```

### Block

A block groups multiple statements between `{` and `}`, separated by `;`:

```c
{
  int x = 1;
  int y = 2;
  print(x + y);
}
```

### If / else

```c
if x > 0 {
  print("positive");
} else {
  print("non-positive");
}
```

The `else` branch is optional. Both branches must be blocks.

### While loop

```c
while i < 10 {
  print(i);
  i = i + 1;
}
```

### Function call (as a statement)

```c
print(result);
```

### Return

```c
return x + 1;
```

Void functions can use `return` with no value, or simply let control reach
the end of the body.

---

## Expressions

### Arithmetic

| Operator | Meaning | Example |
|----------|---------|---------|
| `+` | Addition | `x + 1` |
| `-` | Subtraction / unary minus | `x - y`, `-x` |
| `*` | Multiplication | `x * 2` |
| `/` | Division | `x / 4` |

If one operand is `float` and the other is `int`, the result is `float`.

### Comparison

| Operator | Meaning |
|----------|---------|
| `==` | Equal |
| `!=` | Not equal |
| `<` | Less than |
| `<=` | Less than or equal |
| `>` | Greater than |
| `>=` | Greater than or equal |

Comparison always produces a `bool`.

### Boolean

| Operator | Meaning | Example |
|----------|---------|---------|
| `and` | Logical and (short-circuit) | `x > 0 and x < 10` |
| `or` | Logical or (short-circuit) | `x < 0 or x > 100` |
| `!` | Logical not | `!flag` |

### Operator Precedence

From highest (evaluated first) to lowest (evaluated last):

| Priority | Operators |
|----------|-----------|
| 1 (highest) | function call, array index `[]`, parentheses `()` |
| 2 | unary minus `-`, logical not `!` |
| 3 | `*`, `/` |
| 4 | `+`, `-` |
| 5 | `==`, `!=`, `<`, `<=`, `>`, `>=` |
| 6 | `and` |
| 7 (lowest) | `or` |

Use parentheses to override precedence: `(a + b) * c`.

All binary operators at the same level are **left-associative**:
`1 - 2 - 3` means `(1 - 2) - 3`.

### Array literals and indexing

```c
int[] arr = [10, 20, 30]
int x = arr[0]          -- x is 10
arr[1] = 99
```

---

## Built-in Functions

MiniC provides a small standard library:

| Function | Signature | Description |
|----------|-----------|-------------|
| `print(x)` | `(any) → void` | Print a value to stdout |
| `readInt()` | `() → int` | Read an integer from stdin |
| `readFloat()` | `() → float` | Read a float from stdin |
| `readString()` | `() → str` | Read a line of text from stdin |
| `sqrt(x)` | `(float) → float` | Square root |
| `pow(base, exp)` | `(float, float) → float` | Exponentiation |

---

## Complete Example: Factorial

Here is a complete MiniC program that computes `10!` and prints the result:

```c
int factorial(int n) {
  if n <= 1 { return 1; }
  return n * factorial(n - 1);
}

void main() {
  int result = factorial(10);
  print(result);
}
```

Expected output: `3628800`

Notice:
- `factorial` calls itself recursively — MiniC supports recursion.
- `main` is `void` and takes no parameters — this is required.
- The body of `factorial` is a single `if` statement (no braces needed for a
  single statement).
- `main` uses a block so it can have two statements.

---

## Actor Processes and Message Passing

MiniC supports concurrency via the **actor model**. Programs can declare
isolated `process` definitions that share no mutable memory and communicate
exclusively by sending and receiving tagged messages:

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
  send c ! inc(2);
  send c ! get(self);
  receive {
    value(int v) => { print(v); }
  }
  send c ! stop();
}
```

- `process name(params) { ... }` declares an actor process (no return type).
- `spawn name(args)` starts a new concurrent process and evaluates to its `pid`.
- `self` evaluates to the `pid` of the currently executing process (`<pid 0>` for `main`).
- `send target ! tag(args);` asynchronously copies `args` and appends `tag(args)` to `target`'s mailbox.
- `receive { tag(params) => { ... } ... }` selectively consumes the oldest matching message in the current process's mailbox, suspending the process without busy-waiting until a matching message arrives.

### Server loops with tail recursion (Erlang style)

A `receive` handles exactly one message, and a process ends when its body
ends. Instead of a `while running` loop, a process can call **itself** as its
last statement to handle the next message, passing its state as arguments:

```c
process counter(int n) {
  receive {
    inc(int k) => { counter(n + k); }
    get(pid from) => {
      send from ! value(n);
      counter(n);
    }
    stop() => { }          // no self-call: the process ends
  }
}
```

- The self-call does **not** create a new process: it keeps the same `pid`
  and the same mailbox, and runs the body again with the new arguments.
- It must be in **tail position** (the last statement executed): the last
  statement of a block, of an `if` branch, or of a `receive` clause. It is
  not allowed inside a `while` or followed by other statements.
- Calling a *different* process still requires `spawn`.

---

**What to read next →** [02-pipeline.md](02-pipeline.md)
