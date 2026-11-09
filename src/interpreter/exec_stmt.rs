//! Statement executor for the MiniC interpreter.
//!
//! # Overview
//!
//! Exposes one public function:
//!
//! * [`exec_stmt`] — executes a [`CheckedStmt`] within the current process
//!   context ([`ProcCtx`]) and returns an [`ExecResult`]:
//!   * `Ok(None)` — the statement completed normally (no early return).
//!   * `Ok(Some(value))` — a `return` statement was hit; the value is
//!     propagated up to the caller (`eval_call` in `eval_expr`).
//!   * `Err(RuntimeError)` — a runtime error occurred.
//!
//! Also defines `ExecResult`, the return type alias used throughout.
//!
//! # Design Decisions
//!
//! ## `Option<Value>` to signal early return
//!
//! Statements do not inherently produce values, but a `return` statement
//! inside a function body must pass its value back through potentially many
//! levels of nested `exec_stmt` calls (blocks inside loops inside `receive`
//! clauses, etc.). Using `Option<Value>` as the success case of `ExecResult`
//! encodes this cleanly: `None` means "keep going", `Some(v)` means "stop and
//! return this value".
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

/// `None` = normal fall-through; `Some(v)` = early return with value.
pub type ExecResult = Result<Option<Value>, RuntimeError>;

/// Execute a checked statement. Returns `Some(v)` if a `return` was hit.
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
            Ok(None)
        }

        // --- Assignment ---
        Statement::Assign { target, value } => {
            let val = eval_expr(value, env, ctx)?;
            assign_lvalue(target, val, env, ctx)?;
            Ok(None)
        }

        // --- Block ---
        // Only remove variables declared inside the block on exit.
        // Assignments to outer-scope variables must persist (e.g., loop counters).
        Statement::Block { seq } => {
            let outer_keys = env.names();
            for s in seq {
                match exec_stmt(s, env, ctx) {
                    Ok(Some(ret)) => {
                        env.remove_new(&outer_keys);
                        return Ok(Some(ret));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        env.remove_new(&outer_keys);
                        return Err(e);
                    }
                }
            }
            env.remove_new(&outer_keys);
            Ok(None)
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
                    Ok(None)
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
                    if let Some(ret) = exec_stmt(body, env, ctx)? {
                        return Ok(Some(ret));
                    }
                }
                Value::Bool(false) => return Ok(None),
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
            Ok(Some(val))
        }
        Statement::Return(None) => Ok(Some(Value::Void)),

        // --- Statement-level function call ---
        Statement::Call { name, args } => {
            let arg_vals: Result<Vec<Value>, RuntimeError> =
                args.iter().map(|a| eval_expr(a, env, ctx)).collect();
            eval_call(name, arg_vals?, env, ctx)?;
            Ok(None)
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
            Ok(None)
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
