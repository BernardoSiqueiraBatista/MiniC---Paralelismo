//! Type checker implementation for MiniC.
//!
//! # Overview
//!
//! Provides [`type_check`], which walks an [`UncheckedProgram`] and either
//! returns a [`CheckedProgram`] (every node annotated with its [`Type`]) or
//! a [`TypeError`] describing the first violation found.
//!
//! Also defines [`TypeError`], the error type returned on failure.
//!
//! # Design Decisions
//!
//! ## Using `Environment<Type>` for variable tracking
//!
//! The type checker stores the *declared type* of every in-scope name in an
//! [`Environment<Type>`](crate::environment::Environment). Here `Type` is the
//! MiniC type (e.g., `Type::Int`), not a Rust type. This is the same
//! `Environment` struct used by the interpreter — but instantiated with
//! `Type` instead of `Value`. Functions are also stored in this environment
//! as `Type::Fun(param_types, return_type)` and processes as
//! `Type::Process(param_types)`, so the same lookup mechanism handles
//! variable, function, and process name resolution.
//!
//! ## Function and process signatures registered before bodies are checked
//!
//! All function and process signatures are added to the environment before any
//! body is checked. This allows mutual recursion and allows functions and
//! processes to spawn any declared process without forward declarations. A
//! `fn_snapshot` of the top-level environment is taken after registration and
//! restored at the start of each function or process body check, ensuring
//! variable bindings from one declaration do not leak into another.
//!
//! ## Actor message protocol table
//!
//! Because `pid` is untyped (any variable of type `pid` may hold the address
//! of any process), static message safety is enforced via a program-wide
//! protocol table mapping each message tag `t` to its parameter types
//! `(T1, ..., Tn)`, collected from all `receive` clauses in the program:
//!
//! * If two `receive` clauses use the same tag with different parameter types,
//!   type checking fails with an inconsistent-signature error.
//! * Every `send target ! t(args);` must target a `pid`, refer to a tag `t`
//!   present in the protocol table, and pass compatible argument types.
//!
//! ## Block scoping via `snapshot` / `restore`
//!
//! When the type checker enters a block statement (or a `receive` clause), it
//! takes a snapshot of the current environment. When the scope exits, it
//! restores the snapshot, discarding any variables declared inside.
//!
//! ## `Type::Any` and `types_compatible`
//!
//! The `types_compatible` function implements MiniC's assignability rules,
//! including `Int`↔`Float` coercion, `Pid` compatibility, and the `Any`
//! wildcard used by `print`.

use std::collections::{HashMap, HashSet};

use crate::environment::Environment;
use crate::ir::ast::{
    CheckedExpr, CheckedFunDecl, CheckedProcessDecl, CheckedProgram, CheckedStmt, Expr, ExprD,
    FunDecl, Literal, Message, ProcessDecl, Program, ReceiveClause, Statement, StatementD, Type,
    UncheckedExpr, UncheckedFunDecl, UncheckedProcessDecl, UncheckedProgram, UncheckedStmt,
};
use crate::stdlib::NativeRegistry;

/// Global protocol table: maps each message tag to its declared parameter types.
type MessageTable = HashMap<String, Vec<Type>>;

/// A type error reported by the type checker.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub message: String,
}

impl TypeError {
    pub fn new(msg: impl Into<String>) -> Self {
        Self {
            message: msg.into(),
        }
    }
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for TypeError {}

/// Type-check a program. Returns `Ok(CheckedProgram)` if well-typed, `Err(TypeError)` on first error.
/// Requires a `main` function with signature `void main()`.
pub fn type_check(program: &UncheckedProgram) -> Result<CheckedProgram, TypeError> {
    if program.processes.iter().any(|p| p.name == "main") {
        return Err(TypeError::new("'main' must be a function, not a process"));
    }

    let main_fn = program.main_function();
    match main_fn {
        None => return Err(TypeError::new("program must have a main function")),
        Some(f) => {
            if f.return_type != Type::Unit {
                return Err(TypeError::new("main function must return void"));
            }
            if !f.params.is_empty() {
                return Err(TypeError::new("main function must have no parameters"));
            }
        }
    }

    let registry = NativeRegistry::default();
    check_top_level_names(program, &registry)?;

    let mut env = Environment::<Type>::new();

    // Register native stdlib functions as Type::Fun bindings.
    for (name, entry) in registry.iter() {
        env.declare(
            name.clone(),
            Type::Fun(entry.params.clone(), Box::new(entry.return_type.clone())),
        );
    }

    // Register user-defined function signatures as Type::Fun bindings.
    for f in &program.functions {
        let param_tys = f.params.iter().map(|(_, ty)| ty.clone()).collect();
        env.declare(
            f.name.clone(),
            Type::Fun(param_tys, Box::new(f.return_type.clone())),
        );
    }

    // Register user-defined process signatures as Type::Process bindings.
    for p in &program.processes {
        let param_tys = p.params.iter().map(|(_, ty)| ty.clone()).collect();
        env.declare(p.name.clone(), Type::Process(param_tys));
    }

    // Clean snapshot: only function and process bindings, no variable bindings.
    let fn_snapshot = env.snapshot();

    // Collect global message protocol signatures from all receive clauses.
    let mut msgs = MessageTable::new();
    for f in &program.functions {
        collect_messages(&f.body, &mut msgs)?;
    }
    for p in &program.processes {
        collect_messages(&p.body, &mut msgs)?;
    }

    let mut functions = Vec::new();
    for f in &program.functions {
        let checked = type_check_fun_decl(f, &mut env, &fn_snapshot, &msgs)?;
        functions.push(checked);
    }

    let mut processes = Vec::new();
    for p in &program.processes {
        let checked = type_check_process_decl(p, &mut env, &fn_snapshot, &msgs)?;
        processes.push(checked);
    }

    Ok(Program {
        functions,
        processes,
    })
}

fn check_top_level_names(
    program: &UncheckedProgram,
    registry: &NativeRegistry,
) -> Result<(), TypeError> {
    let mut seen = HashSet::new();
    for f in &program.functions {
        if registry.lookup(&f.name).is_some() {
            return Err(TypeError::new(format!(
                "cannot redefine built-in function '{}'",
                f.name
            )));
        }
        if !seen.insert(f.name.clone()) {
            return Err(TypeError::new(format!("duplicate declaration: '{}'", f.name)));
        }
    }
    for p in &program.processes {
        if registry.lookup(&p.name).is_some() {
            return Err(TypeError::new(format!(
                "cannot redefine built-in function '{}'",
                p.name
            )));
        }
        if !seen.insert(p.name.clone()) {
            return Err(TypeError::new(format!("duplicate declaration: '{}'", p.name)));
        }
    }
    Ok(())
}

fn collect_messages(s: &UncheckedStmt, table: &mut MessageTable) -> Result<(), TypeError> {
    match &s.stmt {
        Statement::Receive { clauses } => {
            for c in clauses {
                let sig: Vec<Type> = c.params.iter().map(|(_, ty)| ty.clone()).collect();
                match table.get(&c.tag) {
                    Some(prev) if *prev != sig => {
                        return Err(TypeError::new(format!(
                            "message '{}' is received with inconsistent signatures: {:?} and {:?}",
                            c.tag, prev, sig
                        )));
                    }
                    Some(_) => {}
                    None => {
                        table.insert(c.tag.clone(), sig);
                    }
                }
                collect_messages(&c.body, table)?;
            }
            Ok(())
        }
        Statement::Block { seq } => seq.iter().try_for_each(|st| collect_messages(st, table)),
        Statement::If {
            then_branch,
            else_branch,
            ..
        } => {
            collect_messages(then_branch, table)?;
            if let Some(eb) = else_branch {
                collect_messages(eb, table)?;
            }
            Ok(())
        }
        Statement::While { body, .. } => collect_messages(body, table),
        Statement::Decl { .. }
        | Statement::Assign { .. }
        | Statement::Call { .. }
        | Statement::Return(_)
        | Statement::Send { .. }
        | Statement::Recur { .. } => Ok(()),
    }
}

/// Verify that every `Recur` (self-call of a process) is in tail position:
/// it must be the last statement executed by the process body.
///
/// Tail position propagates into the last statement of a block, both
/// branches of an `if`, and every clause body of a `receive`. It never
/// propagates into a `while` body, since the loop would run again afterwards.
/// This guarantees that the runtime can restart the process instead of
/// nesting calls, so a server can loop forever without growing the stack.
fn check_tail_recursion(s: &CheckedStmt, in_tail: bool, process: &str) -> Result<(), TypeError> {
    match &s.stmt {
        Statement::Recur { .. } if !in_tail => Err(TypeError::new(format!(
            "recursive call to process '{}' must be its last statement (tail position)",
            process
        ))),
        Statement::Block { seq } => {
            let last = seq.len().saturating_sub(1);
            seq.iter()
                .enumerate()
                .try_for_each(|(i, st)| check_tail_recursion(st, in_tail && i == last, process))
        }
        Statement::If {
            then_branch,
            else_branch,
            ..
        } => {
            check_tail_recursion(then_branch, in_tail, process)?;
            if let Some(eb) = else_branch {
                check_tail_recursion(eb, in_tail, process)?;
            }
            Ok(())
        }
        Statement::While { body, .. } => check_tail_recursion(body, false, process),
        Statement::Receive { clauses } => clauses
            .iter()
            .try_for_each(|c| check_tail_recursion(&c.body, in_tail, process)),
        Statement::Recur { .. }
        | Statement::Decl { .. }
        | Statement::Assign { .. }
        | Statement::Call { .. }
        | Statement::Return(_)
        | Statement::Send { .. } => Ok(()),
    }
}

/// Check arity and argument types of a process instantiation (`spawn p(args)`
/// or a tail-recursive `p(args);` inside process `p`).
fn check_process_args(
    process: &str,
    args: &[CheckedExpr],
    env: &Environment<Type>,
) -> Result<(), TypeError> {
    match env.get(process) {
        Some(Type::Process(param_tys)) => {
            if args.len() != param_tys.len() {
                return Err(TypeError::new(format!(
                    "process '{}' expects {} arguments, got {}",
                    process,
                    param_tys.len(),
                    args.len()
                )));
            }
            for (i, (arg, param_ty)) in args.iter().zip(param_tys.iter()).enumerate() {
                if !types_compatible(&arg.ty, param_ty) {
                    return Err(TypeError::new(format!(
                        "argument {} to process {}: expected {:?}, got {:?}",
                        i + 1,
                        process,
                        param_ty,
                        arg.ty
                    )));
                }
            }
            Ok(())
        }
        Some(Type::Fun(_, _)) => Err(TypeError::new(format!(
            "'{}' is a function, not a process; call it directly",
            process
        ))),
        Some(_) => Err(TypeError::new(format!("'{}' is not a process", process))),
        None => Err(TypeError::new(format!("undefined process: {}", process))),
    }
}

fn type_check_fun_decl(
    f: &UncheckedFunDecl,
    env: &mut Environment<Type>,
    fn_snapshot: &HashMap<String, Type>,
    msgs: &MessageTable,
) -> Result<CheckedFunDecl, TypeError> {
    // Restore to clean top-level state, then add parameters.
    env.restore(fn_snapshot.clone());
    let mut seen_params = HashSet::new();
    for (name, ty) in &f.params {
        if *ty == Type::Unit {
            return Err(TypeError::new(format!(
                "cannot declare parameter '{}' of type void",
                name
            )));
        }
        if !seen_params.insert(name.clone()) {
            return Err(TypeError::new(format!(
                "duplicate parameter '{}' in function '{}'",
                name, f.name
            )));
        }
        env.declare(name.clone(), ty.clone());
    }
    let body = type_check_stmt(&f.body, env, &f.return_type, msgs, None)?;
    Ok(FunDecl {
        name: f.name.clone(),
        params: f.params.clone(),
        return_type: f.return_type.clone(),
        body: Box::new(body),
    })
}

fn type_check_process_decl(
    p: &UncheckedProcessDecl,
    env: &mut Environment<Type>,
    fn_snapshot: &HashMap<String, Type>,
    msgs: &MessageTable,
) -> Result<CheckedProcessDecl, TypeError> {
    env.restore(fn_snapshot.clone());
    let mut seen_params = HashSet::new();
    for (name, ty) in &p.params {
        if *ty == Type::Unit {
            return Err(TypeError::new(format!(
                "cannot declare parameter '{}' of type void",
                name
            )));
        }
        if !seen_params.insert(name.clone()) {
            return Err(TypeError::new(format!(
                "duplicate parameter '{}' in process '{}'",
                name, p.name
            )));
        }
        env.declare(name.clone(), ty.clone());
    }
    let body = type_check_stmt(&p.body, env, &Type::Unit, msgs, Some(&p.name))?;
    check_tail_recursion(&body, true, &p.name)?;
    Ok(ProcessDecl {
        name: p.name.clone(),
        params: p.params.clone(),
        body: Box::new(body),
    })
}

fn type_check_stmt(
    s: &UncheckedStmt,
    env: &mut Environment<Type>,
    expected_return: &Type,
    msgs: &MessageTable,
    current_process: Option<&str>,
) -> Result<CheckedStmt, TypeError> {
    let stmt = match &s.stmt {
        Statement::Decl { name, ty, init } => {
            if ty == &Type::Unit {
                return Err(TypeError::new("cannot declare variable of type void"));
            }
            if env.get(name).is_some() {
                return Err(TypeError::new(format!("redeclaration of variable: {}", name)));
            }
            let init_checked = type_check_expr_to_typed(init, env)?;
            if !types_compatible(&init_checked.ty, ty) {
                return Err(TypeError::new(format!(
                    "declaration of {}: expected {:?}, got {:?}",
                    name, ty, init_checked.ty
                )));
            }
            env.declare(name.clone(), ty.clone());
            Statement::Decl {
                name: name.clone(),
                ty: ty.clone(),
                init: Box::new(init_checked),
            }
        }
        Statement::Assign { target, value } => {
            let value_checked = type_check_expr_to_typed(value, env)?;
            type_check_assign_target(&target.exp, &value_checked.ty, env)?;
            Statement::Assign {
                target: Box::new(type_check_expr_to_typed(target, env)?),
                value: Box::new(value_checked),
            }
        }
        Statement::Block { seq } => {
            let snapshot = env.snapshot();
            let mut checked = Vec::new();
            for st in seq {
                checked.push(type_check_stmt(st, env, expected_return, msgs, current_process)?);
            }
            env.restore(snapshot);
            Statement::Block { seq: checked }
        }
        Statement::Call { name, args } => {
            let args_checked: Result<Vec<_>, _> = args
                .iter()
                .map(|a| type_check_expr_to_typed(a, env))
                .collect();
            let args_checked = args_checked?;
            if current_process == Some(name.as_str()) {
                // `counter(n + k);` inside process `counter`: a tail-recursive
                // restart of the same process (same pid and mailbox).
                // Tail position is verified afterwards by `check_tail_recursion`.
                check_process_args(name, &args_checked, env)?;
                Statement::Recur {
                    process: name.clone(),
                    args: args_checked,
                }
            } else {
                check_call(name, &args_checked, env)?;
                Statement::Call {
                    name: name.clone(),
                    args: args_checked,
                }
            }
        }
        Statement::If {
            cond,
            then_branch,
            else_branch,
        } => {
            let cond_checked = type_check_expr_to_typed(cond, env)?;
            if cond_checked.ty != Type::Bool {
                return Err(TypeError::new(format!(
                    "if condition must be Bool, got {:?}",
                    cond_checked.ty
                )));
            }
            let then_checked = type_check_stmt(then_branch, env, expected_return, msgs, current_process)?;
            let else_checked = else_branch
                .as_ref()
                .map(|e| type_check_stmt(e, env, expected_return, msgs, current_process))
                .transpose()?;
            Statement::If {
                cond: Box::new(cond_checked),
                then_branch: Box::new(then_checked),
                else_branch: else_checked.map(Box::new),
            }
        }
        Statement::While { cond, body } => {
            let cond_checked = type_check_expr_to_typed(cond, env)?;
            if cond_checked.ty != Type::Bool {
                return Err(TypeError::new(format!(
                    "while condition must be Bool, got {:?}",
                    cond_checked.ty
                )));
            }
            let body_checked = type_check_stmt(body, env, expected_return, msgs, current_process)?;
            Statement::While {
                cond: Box::new(cond_checked),
                body: Box::new(body_checked),
            }
        }
        Statement::Return(expr) => match expr {
            None => {
                if *expected_return != Type::Unit {
                    return Err(TypeError::new(format!(
                        "non-void function must return a value of type {:?}",
                        expected_return
                    )));
                }
                Statement::Return(None)
            }
            Some(e) => {
                if *expected_return == Type::Unit {
                    return Err(TypeError::new("void function must not return a value"));
                }
                let checked = type_check_expr_to_typed(e, env)?;
                if !types_compatible(&checked.ty, expected_return) {
                    return Err(TypeError::new(format!(
                        "return type mismatch: expected {:?}, got {:?}",
                        expected_return, checked.ty
                    )));
                }
                Statement::Return(Some(Box::new(checked)))
            }
        },
        Statement::Send { target, message } => {
            let target_checked = type_check_expr_to_typed(target, env)?;
            if target_checked.ty != Type::Pid {
                return Err(TypeError::new(format!(
                    "send target must be Pid, got {:?}",
                    target_checked.ty
                )));
            }
            let sig = msgs.get(&message.tag).ok_or_else(|| {
                TypeError::new(format!(
                    "message '{}' is never received by any process",
                    message.tag
                ))
            })?;
            let args_checked: Vec<CheckedExpr> = message
                .args
                .iter()
                .map(|a| type_check_expr_to_typed(a, env))
                .collect::<Result<_, _>>()?;
            if args_checked.len() != sig.len() {
                return Err(TypeError::new(format!(
                    "message '{}' expects {} arguments, got {}",
                    message.tag,
                    sig.len(),
                    args_checked.len()
                )));
            }
            for (i, (arg, param_ty)) in args_checked.iter().zip(sig.iter()).enumerate() {
                if !types_compatible(&arg.ty, param_ty) {
                    return Err(TypeError::new(format!(
                        "argument {} of message '{}': expected {:?}, got {:?}",
                        i + 1,
                        message.tag,
                        param_ty,
                        arg.ty
                    )));
                }
            }
            Statement::Send {
                target: Box::new(target_checked),
                message: Message {
                    tag: message.tag.clone(),
                    args: args_checked,
                },
            }
        }
        Statement::Receive { clauses } => {
            let mut seen_tags = HashSet::new();
            let mut checked_clauses = Vec::new();
            for clause in clauses {
                if !seen_tags.insert(clause.tag.clone()) {
                    return Err(TypeError::new(format!(
                        "duplicate clause for message '{}' in receive",
                        clause.tag
                    )));
                }
                let snapshot = env.snapshot();
                let mut seen_params = HashSet::new();
                for (param_name, param_ty) in &clause.params {
                    if !seen_params.insert(param_name.clone()) {
                        return Err(TypeError::new(format!(
                            "duplicate parameter '{}' in clause '{}'",
                            param_name, clause.tag
                        )));
                    }
                    if *param_ty == Type::Unit {
                        return Err(TypeError::new(format!(
                            "cannot declare parameter '{}' of type void",
                            param_name
                        )));
                    }
                    if env.get(param_name).is_some() {
                        return Err(TypeError::new(format!(
                            "receive parameter '{}' shadows an existing name",
                            param_name
                        )));
                    }
                    env.declare(param_name.clone(), param_ty.clone());
                }
                let body_checked = type_check_stmt(&clause.body, env, expected_return, msgs, current_process)?;
                env.restore(snapshot);
                checked_clauses.push(ReceiveClause {
                    tag: clause.tag.clone(),
                    params: clause.params.clone(),
                    body: Box::new(body_checked),
                });
            }
            Statement::Receive {
                clauses: checked_clauses,
            }
        }
        Statement::Recur { .. } => {
            return Err(TypeError::new(
                "internal error: Recur is produced by the type checker, not the parser",
            ))
        }
    };
    Ok(StatementD {
        stmt,
        ty: Type::Unit,
    })
}

fn check_call(
    name: &str,
    args: &[CheckedExpr],
    env: &Environment<Type>,
) -> Result<(), TypeError> {
    match env.get(name) {
        Some(Type::Fun(param_tys, _)) => {
            if args.len() != param_tys.len() {
                return Err(TypeError::new(format!(
                    "function '{}' expects {} arguments, got {}",
                    name,
                    param_tys.len(),
                    args.len()
                )));
            }
            for (i, (arg, param_ty)) in args.iter().zip(param_tys.iter()).enumerate() {
                if !types_compatible(&arg.ty, param_ty) {
                    return Err(TypeError::new(format!(
                        "argument {} to {}: expected {:?}, got {:?}",
                        i + 1,
                        name,
                        param_ty,
                        arg.ty
                    )));
                }
            }
            Ok(())
        }
        Some(Type::Process(_)) => Err(TypeError::new(format!(
            "'{}' is a process; use spawn to start it",
            name
        ))),
        Some(_) => Err(TypeError::new(format!("'{}' is not a function", name))),
        None => Err(TypeError::new(format!("undefined function: {}", name))),
    }
}

fn type_check_assign_target(
    target: &Expr<()>,
    value_ty: &Type,
    env: &Environment<Type>,
) -> Result<(), TypeError> {
    match target {
        Expr::Ident(name) => {
            let declared_ty = env
                .get(name)
                .ok_or_else(|| TypeError::new(format!("undeclared variable: {}", name)))?;
            if matches!(declared_ty, Type::Fun(_, _) | Type::Process(_)) {
                return Err(TypeError::new(format!("cannot assign to '{}'", name)));
            }
            if !types_compatible(value_ty, declared_ty) {
                return Err(TypeError::new(format!(
                    "assignment to {}: expected {:?}, got {:?}",
                    name, declared_ty, value_ty
                )));
            }
            Ok(())
        }
        Expr::Index { base, index } => {
            let index_ty = type_check_expr(index, env)?;
            if index_ty != Type::Int {
                return Err(TypeError::new("array index must be Int"));
            }
            let base_ty = type_check_expr(base, env)?;
            if let Type::Array(elem) = &base_ty {
                if **elem != *value_ty {
                    return Err(TypeError::new("assignment type mismatch"));
                }
            } else {
                return Err(TypeError::new("indexed target must be array"));
            }
            Ok(())
        }
        _ => Err(TypeError::new("invalid assignment target")),
    }
}

fn type_check_expr_to_typed(
    e: &UncheckedExpr,
    env: &Environment<Type>,
) -> Result<CheckedExpr, TypeError> {
    let ty = type_check_expr(e, env)?;
    let exp = type_check_expr_inner(&e.exp, env)?;
    Ok(ExprD { exp, ty })
}

fn type_check_expr_inner(
    e: &Expr<()>,
    env: &Environment<Type>,
) -> Result<Expr<Type>, TypeError> {
    match e {
        Expr::Literal(l) => Ok(Expr::Literal(l.clone())),
        Expr::Ident(name) => Ok(Expr::Ident(name.clone())),
        Expr::Neg(inner) => Ok(Expr::Neg(Box::new(type_check_expr_to_typed(inner, env)?))),
        Expr::Add(l, r) => Ok(Expr::Add(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Sub(l, r) => Ok(Expr::Sub(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Mul(l, r) => Ok(Expr::Mul(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Div(l, r) => Ok(Expr::Div(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Eq(l, r) => Ok(Expr::Eq(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Ne(l, r) => Ok(Expr::Ne(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Lt(l, r) => Ok(Expr::Lt(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Le(l, r) => Ok(Expr::Le(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Gt(l, r) => Ok(Expr::Gt(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Ge(l, r) => Ok(Expr::Ge(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Not(inner) => Ok(Expr::Not(Box::new(type_check_expr_to_typed(inner, env)?))),
        Expr::And(l, r) => Ok(Expr::And(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Or(l, r) => Ok(Expr::Or(
            Box::new(type_check_expr_to_typed(l, env)?),
            Box::new(type_check_expr_to_typed(r, env)?),
        )),
        Expr::Call { name, args } => {
            let args_checked: Result<Vec<_>, _> =
                args.iter().map(|a| type_check_expr_to_typed(a, env)).collect();
            Ok(Expr::Call {
                name: name.clone(),
                args: args_checked?,
            })
        }
        Expr::ArrayLit(elems) => {
            let elems_checked: Result<Vec<_>, _> =
                elems.iter().map(|e| type_check_expr_to_typed(e, env)).collect();
            Ok(Expr::ArrayLit(elems_checked?))
        }
        Expr::Index { base, index } => Ok(Expr::Index {
            base: Box::new(type_check_expr_to_typed(base, env)?),
            index: Box::new(type_check_expr_to_typed(index, env)?),
        }),
        Expr::Spawn { process, args } => {
            let args_checked: Result<Vec<_>, _> =
                args.iter().map(|a| type_check_expr_to_typed(a, env)).collect();
            Ok(Expr::Spawn {
                process: process.clone(),
                args: args_checked?,
            })
        }
        Expr::SelfPid => Ok(Expr::SelfPid),
    }
}

fn type_check_expr(
    e: &UncheckedExpr,
    env: &Environment<Type>,
) -> Result<Type, TypeError> {
    match &e.exp {
        Expr::Literal(l) => Ok(literal_type(l)),
        Expr::Ident(name) => match env.get(name) {
            Some(Type::Fun(_, _)) => Err(TypeError::new(format!(
                "cannot use function '{}' as a value",
                name
            ))),
            Some(Type::Process(_)) => Err(TypeError::new(format!(
                "cannot use process '{}' as a value",
                name
            ))),
            Some(ty) => Ok(ty.clone()),
            None => Err(TypeError::new(format!("undeclared variable: {}", name))),
        },
        Expr::Neg(inner) => {
            let ty = type_check_expr(inner, env)?;
            if matches!(ty, Type::Int | Type::Float) {
                Ok(ty)
            } else {
                Err(TypeError::new("unary minus requires Int or Float"))
            }
        }
        Expr::Add(l, r) | Expr::Sub(l, r) | Expr::Mul(l, r) | Expr::Div(l, r) => {
            let lt = type_check_expr(l, env)?;
            let rt = type_check_expr(r, env)?;
            numeric_binop_result(&lt, &rt)
        }
        Expr::Eq(l, r) | Expr::Ne(l, r) => {
            let lt = type_check_expr(l, env)?;
            let rt = type_check_expr(r, env)?;
            if !types_compatible(&lt, &rt) {
                return Err(TypeError::new(format!(
                    "equality operands must have compatible types, got {:?} and {:?}",
                    lt, rt
                )));
            }
            Ok(Type::Bool)
        }
        Expr::Lt(l, r) | Expr::Le(l, r) | Expr::Gt(l, r) | Expr::Ge(l, r) => {
            let lt = type_check_expr(l, env)?;
            let rt = type_check_expr(r, env)?;
            if !is_numeric(&lt) || !is_numeric(&rt) {
                return Err(TypeError::new(format!(
                    "ordering comparison requires numeric operands, got {:?} and {:?}",
                    lt, rt
                )));
            }
            Ok(Type::Bool)
        }
        Expr::Not(inner) => {
            let ty = type_check_expr(inner, env)?;
            if ty == Type::Bool {
                Ok(Type::Bool)
            } else {
                Err(TypeError::new("not requires Bool operand"))
            }
        }
        Expr::And(l, r) | Expr::Or(l, r) => {
            let lt = type_check_expr(l, env)?;
            let rt = type_check_expr(r, env)?;
            if lt == Type::Bool && rt == Type::Bool {
                Ok(Type::Bool)
            } else {
                Err(TypeError::new("and/or require Bool operands"))
            }
        }
        Expr::Call { name, args } => {
            let args_checked: Result<Vec<_>, _> =
                args.iter().map(|a| type_check_expr_to_typed(a, env)).collect();
            let args_checked = args_checked?;
            match env.get(name) {
                Some(Type::Fun(param_tys, return_ty)) => {
                    if args_checked.len() != param_tys.len() {
                        return Err(TypeError::new(format!(
                            "function '{}' expects {} arguments, got {}",
                            name,
                            param_tys.len(),
                            args_checked.len()
                        )));
                    }
                    for (i, (arg, param_ty)) in
                        args_checked.iter().zip(param_tys.iter()).enumerate()
                    {
                        if !types_compatible(&arg.ty, param_ty) {
                            return Err(TypeError::new(format!(
                                "argument {} to {}: expected {:?}, got {:?}",
                                i + 1,
                                name,
                                param_ty,
                                arg.ty
                            )));
                        }
                    }
                    Ok((**return_ty).clone())
                }
                Some(Type::Process(_)) => Err(TypeError::new(format!(
                    "'{}' is a process; use spawn to start it",
                    name
                ))),
                Some(_) => Err(TypeError::new(format!("'{}' is not a function", name))),
                None => Err(TypeError::new(format!("undefined function: {}", name))),
            }
        }
        Expr::ArrayLit(elems) => {
            if elems.is_empty() {
                return Err(TypeError::new("empty array literal needs type annotation"));
            }
            let first = type_check_expr(&elems[0], env)?;
            for e in elems.iter().skip(1) {
                let ty = type_check_expr(e, env)?;
                if !types_compatible(&first, &ty) {
                    return Err(TypeError::new("array elements must have same type"));
                }
            }
            Ok(Type::Array(Box::new(first)))
        }
        Expr::Index { base, index } => {
            let index_ty = type_check_expr(index, env)?;
            if index_ty != Type::Int {
                return Err(TypeError::new("array index must be Int"));
            }
            let base_ty = type_check_expr(base, env)?;
            if let Type::Array(elem) = base_ty {
                Ok(*elem)
            } else {
                Err(TypeError::new("indexed expression must be array"))
            }
        }
        Expr::Spawn { process, args } => {
            let args_checked: Result<Vec<_>, _> =
                args.iter().map(|a| type_check_expr_to_typed(a, env)).collect();
            check_process_args(process, &args_checked?, env)?;
            Ok(Type::Pid)
        }
        Expr::SelfPid => Ok(Type::Pid),
    }
}

fn literal_type(l: &Literal) -> Type {
    match l {
        Literal::Int(_) => Type::Int,
        Literal::Float(_) => Type::Float,
        Literal::Str(_) => Type::Str,
        Literal::Bool(_) => Type::Bool,
    }
}

fn numeric_binop_result(l: &Type, r: &Type) -> Result<Type, TypeError> {
    match (l, r) {
        (Type::Int, Type::Int) => Ok(Type::Int),
        (Type::Int, Type::Float) | (Type::Float, Type::Int) | (Type::Float, Type::Float) => {
            Ok(Type::Float)
        }
        _ => Err(TypeError::new("arithmetic operands must be Int or Float")),
    }
}

fn is_numeric(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float)
}

fn types_compatible(a: &Type, b: &Type) -> bool {
    match (a, b) {
        // Any parameter accepts any argument type.
        (_, Type::Any) => true,
        (Type::Int, Type::Int)
        | (Type::Float, Type::Float)
        | (Type::Bool, Type::Bool)
        | (Type::Str, Type::Str)
        | (Type::Pid, Type::Pid)
        | (Type::Unit, Type::Unit) => true,
        (Type::Int, Type::Float) | (Type::Float, Type::Int) => true,
        (Type::Array(a), Type::Array(b)) => types_compatible(a, b),
        _ => false,
    }
}
