//! Statement executor for the MiniC interpreter.
//!
//! # Overview
//!
//! Exposes one public function:
//!
//! * [`exec_stmt`] — executes a [`CheckedStmt`] within the current process
//!   context ([`ProcCtx`]) and returns an [`ExecResult`]:
//!   * `Ok(Flow::Normal)` — the statement completed normally.
//!   * `Ok(Flow::Return(value))` — a `return` statement was hit; the value is
//!     propagated up to the caller (`eval_call` in `eval_expr`).
//!   * `Ok(Flow::Recur(args))` — a process called itself in tail position
//!     (`counter(n + k);`); propagated up to `run_process`, which restarts the
//!     process body with `args` (same pid, same mailbox).
//!   * `Err(RuntimeError)` — a runtime error occurred.
//!
//! Also defines [`Flow`] and `ExecResult`, the return type alias used throughout.
//!
//! # Design Decisions
//!
//! ## `Flow` to signal early exit
//!
//! Statements do not inherently produce values, but a `return` (or a
//! tail-recursive process restart) must pass through potentially many levels
//! of nested `exec_stmt` calls (blocks inside `if`s inside `receive` clauses,
//! etc.). Every compound statement stops and propagates any non-`Normal`
//! `Flow` immediately.
//!
//! ## Block and `receive` clause scoping via `names` / `remove_new`
//!
//! When `exec_stmt` enters a `Block` or matches a `Receive` clause, it records
//! the set of names currently bound in the environment (`env.names()`). On
//! exit, it calls `env.remove_new(&outer_keys)` to drop any bindings
//! introduced inside that scope while preserving assignments to outer-scope
//! variables (such as `n = n + k;` or `running = false;` inside an actor loop).

use crate::environment::Environment;
use crate::ir::ast::{CheckedExpr, CheckedStmt, Expr, Statement};

use super::eval_expr::{eval_call, eval_expr};
use super::runtime::{ProcCtx, RtMessage};
use super::value::{RuntimeError, Value};

/// How control leaves a statement.
#[derive(Debug, Clone)]
pub enum Flow {
    /// Fall through to the next statement.
    Normal,
    /// A `return` was executed with this value.
    Return(Value),
    /// The current process asked to restart itself with these arguments.
    Recur(Vec<Value>),
}

pub type ExecResult = Result<Flow, RuntimeError>;

/// Execute a checked statement.
pub fn exec_stmt(
    stmt: &CheckedStmt,
    env: &mut Environment<Value>,
    ctx: &ProcCtx,
) -> ExecResult {
    match &stmt.stmt {
        // --- Variable declaration ---
        Statement::Decl { name, init, .. } => {
            let val = eval_expr(init, env, ctx)?;
            env.declare(name.clone(), val);
            Ok(Flow::Normal)
        }

        // --- Assignment ---
        Statement::Assign { target, value } => {
            let val = eval_expr(value, env, ctx)?;
            assign_lvalue(target, val, env, ctx)?;
            Ok(Flow::Normal)
        }

        // --- Block ---
        // Only remove variables declared inside the block on exit.
        // Assignments to outer-scope variables must persist (e.g., loop counters).
        Statement::Block { seq } => {
            let outer_keys = env.names();
            for s in seq {
                match exec_stmt(s, env, ctx) {
                    Ok(Flow::Normal) => {}
                    other => {
                        env.remove_new(&outer_keys);
                        return other;
                    }
                }
            }
            env.remove_new(&outer_keys);
            Ok(Flow::Normal)
        }

        // --- If ---
        Statement::If {
            cond,
            then_branch,
            else_branch,
        } => match eval_expr(cond, env, ctx)? {
            Value::Bool(true) => exec_stmt(then_branch, env, ctx),
            Value::Bool(false) => {
                if let Some(eb) = else_branch {
                    exec_stmt(eb, env, ctx)
                } else {
                    Ok(Flow::Normal)
                }
            }
            v => Err(RuntimeError::new(format!(
                "if condition must be bool, got: {}",
                v
            ))),
        },

        // --- While ---
        Statement::While { cond, body } => loop {
            match eval_expr(cond, env, ctx)? {
                Value::Bool(true) => {
                    match exec_stmt(body, env, ctx)? {
                        Flow::Normal => {}
                        exit => return Ok(exit),
                    }
                }
                Value::Bool(false) => return Ok(Flow::Normal),
                v => {
                    return Err(RuntimeError::new(format!(
                        "while condition must be bool, got: {}",
                        v
                    )))
                }
            }
        },

        // --- Return ---
        Statement::Return(Some(expr)) => {
            let val = eval_expr(expr, env, ctx)?;
            Ok(Flow::Return(val))
        }
        Statement::Return(None) => Ok(Flow::Return(Value::Void)),

        // --- Statement-level function call ---
        Statement::Call { name, args } => {
            let arg_vals: Result<Vec<Value>, RuntimeError> =
                args.iter().map(|a| eval_expr(a, env, ctx)).collect();
            eval_call(name, arg_vals?, env, ctx)?;
            Ok(Flow::Normal)
        }

        // --- Tail-recursive process restart: `counter(n + k);` ---
        // Arguments are evaluated now; the restart itself happens in
        // `run_process`, after every enclosing statement has unwound.
        Statement::Recur { args, .. } => {
            let arg_vals: Result<Vec<Value>, RuntimeError> =
                args.iter().map(|a| eval_expr(a, env, ctx)).collect();
            Ok(Flow::Recur(arg_vals?))
        }

        // --- Asynchronous message send ---
        Statement::Send { target, message } => {
            let to = match eval_expr(target, env, ctx)? {
                Value::Pid(p) => p,
                v => {
                    return Err(RuntimeError::new(format!(
                        "send target must be a pid, got: {}",
                        v
                    )))
                }
            };
            let arg_vals: Result<Vec<Value>, RuntimeError> = message
                .args
                .iter()
                .map(|a| eval_expr(a, env, ctx))
                .collect();
            ctx.rt.send(
                to,
                RtMessage {
                    tag: message.tag.clone(),
                    args: arg_vals?,
                },
            );
            Ok(Flow::Normal)
        }

        // --- Selective blocking receive ---
        Statement::Receive { clauses } => {
            let tags: Vec<String> = clauses.iter().map(|c| c.tag.clone()).collect();
            let (idx, arg_vals) = ctx.rt.receive(ctx.pid, &tags)?;
            let clause = &clauses[idx];
            if arg_vals.len() != clause.params.len() {
                return Err(RuntimeError::new(format!(
                    "message '{}' expects {} arguments, got {}",
                    clause.tag,
                    clause.params.len(),
                    arg_vals.len()
                )));
            }
            let outer_keys = env.names();
            for ((param_name, _), val) in clause.params.iter().zip(arg_vals) {
                env.declare(param_name.clone(), val);
            }
            let res = exec_stmt(&clause.body, env, ctx);
            env.remove_new(&outer_keys);
            res
        }
    }
}

/// Assign `val` to the lvalue described by `target`.
fn assign_lvalue(
    target: &CheckedExpr,
    val: Value,
    env: &mut Environment<Value>,
    ctx: &ProcCtx,
) -> Result<(), RuntimeError> {
    match &target.exp {
        Expr::Ident(name) => {
            if env.set(name, val) {
                Ok(())
            } else {
                Err(RuntimeError::new(format!(
                    "assignment to undeclared variable '{}'",
                    name
                )))
            }
        }
        Expr::Index { base, index } => {
            let idx = match eval_expr(index, env, ctx)? {
                Value::Int(i) if i >= 0 => i as usize,
                Value::Int(i) => {
                    return Err(RuntimeError::new(format!(
                        "array index {} out of bounds",
                        i
                    )))
                }
                v => {
                    return Err(RuntimeError::new(format!(
                        "array index must be int, got: {}",
                        v
                    )))
                }
            };
            assign_index(base, idx, val, env, ctx)
        }
        _ => Err(RuntimeError::new("invalid assignment target".to_string())),
    }
}

/// Recursively assign into a (possibly nested) array lvalue.
fn assign_index(
    base: &CheckedExpr,
    idx: usize,
    val: Value,
    env: &mut Environment<Value>,
    ctx: &ProcCtx,
) -> Result<(), RuntimeError> {
    match &base.exp {
        Expr::Ident(name) => {
            let arr = env
                .get(name)
                .cloned()
                .ok_or_else(|| RuntimeError::new(format!("undefined variable '{}'", name)))?;
            match arr {
                Value::Array(mut elems) => {
                    if idx >= elems.len() {
                        return Err(RuntimeError::new(format!(
                            "array index {} out of bounds (len {})",
                            idx,
                            elems.len()
                        )));
                    }
                    elems[idx] = val;
                    env.set(name, Value::Array(elems));
                    Ok(())
                }
                v => Err(RuntimeError::new(format!(
                    "cannot index non-array value: {}",
                    v
                ))),
            }
        }
        Expr::Index {
            base: inner_base,
            index: inner_index,
        } => {
            let inner_idx = match eval_expr(inner_index, env, ctx)? {
                Value::Int(i) if i >= 0 => i as usize,
                Value::Int(i) => {
                    return Err(RuntimeError::new(format!(
                        "array index {} out of bounds",
                        i
                    )))
                }
                v => {
                    return Err(RuntimeError::new(format!(
                        "array index must be int, got: {}",
                        v
                    )))
                }
            };
            let outer_name = extract_ident_name(inner_base)?;
            let outer = env
                .get(&outer_name)
                .cloned()
                .ok_or_else(|| RuntimeError::new(format!("undefined variable '{}'", outer_name)))?;
            match outer {
                Value::Array(mut outer_elems) => {
                    if inner_idx >= outer_elems.len() {
                        return Err(RuntimeError::new(format!(
                            "array index {} out of bounds (len {})",
                            inner_idx,
                            outer_elems.len()
                        )));
                    }
                    match &mut outer_elems[inner_idx] {
                        Value::Array(ref mut inner_elems) => {
                            if idx >= inner_elems.len() {
                                return Err(RuntimeError::new(format!(
                                    "array index {} out of bounds (len {})",
                                    idx,
                                    inner_elems.len()
                                )));
                            }
                            inner_elems[idx] = val;
                        }
                        v => {
                            return Err(RuntimeError::new(format!(
                                "cannot index non-array value: {}",
                                v
                            )))
                        }
                    }
                    env.set(&outer_name, Value::Array(outer_elems));
                    Ok(())
                }
                v => Err(RuntimeError::new(format!(
                    "cannot index non-array value: {}",
                    v
                ))),
            }
        }
        _ => Err(RuntimeError::new("invalid assignment target".to_string())),
    }
}

fn extract_ident_name(expr: &CheckedExpr) -> Result<String, RuntimeError> {
    match &expr.exp {
        Expr::Ident(name) => Ok(name.clone()),
        _ => Err(RuntimeError::new(
            "nested array assignment only supported for simple variable bases".to_string(),
        )),
    }
}
