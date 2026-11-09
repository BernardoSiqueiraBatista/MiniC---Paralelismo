//! Tree-walking interpreter and actor runtime for MiniC.
//!
//! # Overview
//!
//! This module is the execution stage of the MiniC pipeline. It takes a
//! [`CheckedProgram`] — guaranteed by the type system to be well-typed — and
//! executes it, starting at `main` (`pid 0`).
//!
//! Public entry point:
//!
//! * [`interpret`] — initializes the concurrent [`runtime::Runtime`], runs
//!   `main`, waits for spawned actor processes to finish (or reach
//!   quiescence), and reports any runtime errors.
//!
//! Internal structure:
//!
//! * [`eval_expr`] — evaluates expressions to [`Value`]s.
//! * [`exec_stmt`] — executes statements, driving `eval_expr` as needed.
//! * [`runtime`] — OS-thread actor runtime with per-process condition
//!   variables, mailboxes, selective `receive`, and deadlock detection.
//! * [`value`] — defines the runtime value type and error type.
//!
//! # Design Decisions
//!
//! ## Tree-walking interpretation with real OS threads for actors
//!
//! Because `exec_stmt` and `eval_expr` use the Rust call stack directly,
//! running each spawned actor process on its own OS thread lets a blocking
//! `receive` suspend that process's call stack natively via its dedicated
//! `Condvar` (zero busy-waiting) while other processes continue executing in
//! parallel.
//!
//! ## Per-process isolated environments
//!
//! Every process starts from a fresh [`base_env`] containing only stdlib
//! functions and user-defined functions. Because MiniC has no global variables
//! or pointers and all values sent in messages are cloned, processes share no
//! mutable memory.

pub mod eval_expr;
pub mod exec_stmt;
pub mod runtime;
pub mod value;

use std::sync::Arc;

use crate::environment::Environment;
use crate::ir::ast::CheckedProgram;
use crate::stdlib::NativeRegistry;

use eval_expr::eval_call;
use runtime::{ProcCtx, Runtime, MAIN_PID};
use value::{FnValue, RuntimeError, Value};

/// Build the initial environment for a process: stdlib natives + user-defined functions.
pub(crate) fn base_env(program: &CheckedProgram) -> Environment<Value> {
    let mut env = Environment::<Value>::new();

    // Register native stdlib functions as Value::Fn(FnValue::Native) bindings.
    let registry = NativeRegistry::default();
    for (name, entry) in registry.iter() {
        env.declare(name.clone(), Value::Fn(FnValue::Native(entry.func)));
    }

    // Register user-defined functions as Value::Fn(FnValue::UserDefined) bindings.
    for fun in &program.functions {
        env.declare(fun.name.clone(), Value::Fn(FnValue::UserDefined(fun.clone())));
    }

    env
}

/// Interpret a type-checked MiniC program, starting execution at `main`.
pub fn interpret(program: &CheckedProgram) -> Result<(), RuntimeError> {
    let mut env = base_env(program);
    if env.get("main").is_none() {
        return Err(RuntimeError::new("no 'main' function found"));
    }

    let rt = Arc::new(Runtime::new(program.clone()));
    let ctx = ProcCtx {
        pid: MAIN_PID,
        rt: Arc::clone(&rt),
    };

    let main_result = eval_call("main", vec![], &mut env, &ctx).map(|_| ());
    let main_err = main_result.as_ref().err().cloned();
    rt.finish(MAIN_PID, main_result);

    let children_result = rt.wait_all();
    match main_err {
        Some(e) => Err(e),
        None => children_result,
    }
}
