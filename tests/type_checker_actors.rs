//! Integration tests for actor type checking (milestone 2).

use std::path::Path;

use mini_c::ir::ast::{CheckedProgram, Type};
use mini_c::parser::program;
use mini_c::semantic::{type_check, TypeError};
use nom::combinator::all_consuming;

fn check(src: &str) -> Result<CheckedProgram, TypeError> {
    let (_, prog) = all_consuming(program)(src.trim())
        .map_err(|e| TypeError::new(format!("parse failed: {:?}", e)))?;
    type_check(&prog)
}

fn check_fixture(name: &str) -> Result<CheckedProgram, TypeError> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let src = std::fs::read_to_string(&path).expect("fixture file should exist");
    check(&src)
}

// --- Well-typed programs ---

#[test]
fn test_fixture_counter_well_typed() {
    let checked = check_fixture("actor_counter.minic").expect("actor_counter should type-check");
    assert_eq!(checked.processes.len(), 1);
    assert_eq!(checked.functions.len(), 1);
}

#[test]
fn test_fixture_pingpong_well_typed() {
    let checked = check_fixture("actor_pingpong.minic").expect("actor_pingpong should type-check");
    assert_eq!(checked.processes.len(), 2);
    assert_eq!(checked.functions.len(), 2);
}

#[test]
fn test_spawn_and_self_have_pid_type() {
    let checked = check(
        r#"
        process worker(int id, float weight) {
            pid me = self;
        }
        void main() {
            pid w = spawn worker(1, 2);
            bool same = w == self;
            bool diff = w != self;
            pid[] group = [self, w];
            print(w);
        }
        "#,
    )
    .expect("should type-check");
    assert_eq!(checked.processes.len(), 1);
}

#[test]
fn test_consistent_message_signatures_across_processes() {
    let res = check(
        r#"
        process a() {
            receive { ping(pid from, int n) => { } }
        }
        process b() {
            receive { ping(pid sender, int seq) => { } }
        }
        void main() {
            pid pa = spawn a();
            pid pb = spawn b();
            send pa ! ping(self, 1);
            send pb ! ping(self, 2);
        }
        "#,
    );
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_empty_return_allowed_in_process() {
    let res = check(
        r#"
        process early() {
            return;
        }
        void main() {
            pid p = spawn early();
        }
        "#,
    );
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_int_to_float_coercion_in_message_args() {
    let res = check(
        r#"
        process sink() {
            receive { measure(float val) => { print(val); } }
        }
        void main() {
            pid s = spawn sink();
            send s ! measure(42);
        }
        "#,
    );
    assert!(res.is_ok(), "{:?}", res.err());
}

// --- Static error cases ---

#[test]
fn test_spawn_undefined_process() {
    let err = check("void main() { pid p = spawn ghost(); }").unwrap_err();
    assert!(err.message.contains("undefined process"), "{}", err.message);
}

#[test]
fn test_spawn_function_rejected() {
    let err = check(
        r#"
        void helper() { }
        void main() { pid p = spawn helper(); }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("is a function"), "{}", err.message);
}

#[test]
fn test_call_process_as_function_rejected() {
    let err = check(
        r#"
        process worker(int x) { }
        void main() { worker(1); }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("is a process"), "{}", err.message);
}

#[test]
fn test_call_process_in_expr_rejected() {
    let err = check(
        r#"
        process worker(int x) { }
        void main() { int y = worker(1); }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("is a process"), "{}", err.message);
}

#[test]
fn test_process_used_as_value_rejected() {
    let err = check(
        r#"
        process worker() { }
        void main() { pid p = worker; }
        "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("cannot use process"),
        "{}",
        err.message
    );
}

#[test]
fn test_spawn_wrong_arity_and_arg_type() {
    let err_arity = check(
        r#"
        process worker(int a, int b) { }
        void main() { pid p = spawn worker(1); }
        "#,
    )
    .unwrap_err();
    assert!(
        err_arity.message.contains("expects 2 arguments, got 1"),
        "{}",
        err_arity.message
    );

    let err_ty = check(
        r#"
        process worker(int a) { }
        void main() { pid p = spawn worker("bad"); }
        "#,
    )
    .unwrap_err();
    assert!(
        err_ty.message.contains("argument 1 to process worker"),
        "{}",
        err_ty.message
    );
}

#[test]
fn test_assign_spawn_to_non_pid_rejected() {
    let err = check(
        r#"
        process worker() { }
        void main() { int x = spawn worker(); }
        "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("expected Int, got Pid"),
        "{}",
        err.message
    );
}

#[test]
fn test_send_non_pid_target_rejected() {
    let err = check(
        r#"
        process sink() { receive { m() => { } } }
        void main() { send 42 ! m(); }
        "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("send target must be Pid"),
        "{}",
        err.message
    );
}

#[test]
fn test_send_unhandled_message_tag_rejected() {
    let err = check("void main() { send self ! unknown(1); }").unwrap_err();
    assert!(err.message.contains("never received"), "{}", err.message);
}

#[test]
fn test_send_message_wrong_arity_and_type() {
    let err_arity = check(
        r#"
        process sink() { receive { item(int x) => { } } }
        void main() {
            pid s = spawn sink();
            send s ! item(1, 2);
        }
        "#,
    )
    .unwrap_err();
    assert!(
        err_arity
            .message
            .contains("message 'item' expects 1 arguments, got 2"),
        "{}",
        err_arity.message
    );

    let err_ty = check(
        r#"
        process sink() { receive { item(int x) => { } } }
        void main() {
            pid s = spawn sink();
            send s ! item("wrong");
        }
        "#,
    )
    .unwrap_err();
    assert!(
        err_ty.message.contains("argument 1 of message 'item'"),
        "{}",
        err_ty.message
    );
}

#[test]
fn test_inconsistent_message_signatures_rejected() {
    let err = check(
        r#"
        process a() { receive { msg(int x) => { } } }
        process b() { receive { msg(str s) => { } } }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("inconsistent signatures"),
        "{}",
        err.message
    );
}

#[test]
fn test_duplicate_clause_in_same_receive_rejected() {
    let err = check(
        r#"
        process a() {
            receive {
                ping(int x) => { }
                ping(int y) => { }
            }
        }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("duplicate clause"), "{}", err.message);
}

#[test]
fn test_duplicate_param_and_shadowing_in_clause_rejected() {
    let err_dup = check(
        r#"
        process a() {
            receive { pair(int x, int x) => { } }
        }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(
        err_dup.message.contains("duplicate parameter"),
        "{}",
        err_dup.message
    );

    let err_shadow = check(
        r#"
        process a(int count) {
            receive { inc(int count) => { } }
        }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err_shadow.message.contains("shadows"), "{}", err_shadow.message);
}

#[test]
fn test_duplicate_top_level_names_and_process_main_rejected() {
    let err_main = check("process main() { }").unwrap_err();
    assert!(err_main.message.contains("main"), "{}", err_main.message);

    let err_collide = check(
        r#"
        void worker() { }
        process worker() { }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(
        err_collide.message.contains("duplicate declaration"),
        "{}",
        err_collide.message
    );

    let err_dup_proc = check(
        r#"
        process worker() { }
        process worker() { }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(
        err_dup_proc.message.contains("duplicate declaration"),
        "{}",
        err_dup_proc.message
    );
}

#[test]
fn test_process_cannot_return_value() {
    let err = check(
        r#"
        process bad() { return 1; }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(
        err.message.contains("must not return a value"),
        "{}",
        err.message
    );
}

#[test]
fn test_pid_arithmetic_and_ordering_rejected() {
    let err_arith = check("void main() { int x = self + 1; }").unwrap_err();
    assert!(
        err_arith.message.contains("Int or Float"),
        "{}",
        err_arith.message
    );

    let err_ord = check("void main() { bool b = self < self; }").unwrap_err();
    assert!(err_ord.message.contains("numeric"), "{}", err_ord.message);
}

#[test]
fn test_checked_spawn_has_pid_type_annotation() {
    let checked = check(
        r#"
        process p() { }
        void main() { pid x = spawn p(); }
        "#,
    )
    .unwrap();
    let main_fn = checked.main_function().unwrap();
    if let mini_c::ir::ast::Statement::Block { ref seq } = main_fn.body.stmt {
        if let mini_c::ir::ast::Statement::Decl { ref init, .. } = seq[0].stmt {
            assert_eq!(init.ty, Type::Pid);
        } else {
            panic!("expected Decl");
        }
    } else {
        panic!("expected Block");
    }
}

// --- Tail-recursive processes (Erlang-style server loop) ---

#[test]
fn test_fixture_counter_recursive_well_typed() {
    let checked = check_fixture("actor_counter_recursive.minic")
        .expect("actor_counter_recursive should type-check");
    assert_eq!(checked.processes.len(), 1);
}

#[test]
fn test_tail_self_call_becomes_recur() {
    let checked = check(
        r#"
        process loop_forever(int n) {
            receive {
                tick() => { loop_forever(n + 1); }
            }
        }
        void main() { send spawn loop_forever(0) ! tick(); }
        "#,
    )
    .expect("tail self-call should type-check");
    let body = format!("{:?}", checked.processes[0].body);
    assert!(body.contains("Recur"), "{}", body);
}

#[test]
fn test_tail_self_call_allowed_in_both_if_branches() {
    check(
        r#"
        process p(int n) {
            if n > 0 { p(n - 1); } else { p(n + 1); }
        }
        void main() { }
        "#,
    )
    .expect("self-call in both if branches is in tail position");
}

#[test]
fn test_self_call_not_last_is_rejected() {
    let err = check(
        r#"
        process p(int n) {
            p(n + 1);
            print(n);
        }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("tail position"), "{}", err.message);
}

#[test]
fn test_self_call_inside_while_is_rejected() {
    let err = check(
        r#"
        process p(int n) {
            while true { p(n + 1); }
        }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("tail position"), "{}", err.message);
}

#[test]
fn test_self_call_followed_by_statement_in_clause_is_rejected() {
    let err = check(
        r#"
        process p(int n) {
            receive {
                tick() => { p(n + 1); }
            }
            print(n);
        }
        void main() { send self ! tick(); }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("tail position"), "{}", err.message);
}

#[test]
fn test_self_call_wrong_arguments_rejected() {
    let arity = check(
        r#"
        process p(int n) { p(); }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(arity.message.contains("expects 1 arguments"), "{}", arity.message);

    let ty = check(
        r#"
        process p(int n) { p(true); }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(ty.message.contains("expected Int"), "{}", ty.message);
}

#[test]
fn test_calling_another_process_still_requires_spawn() {
    let err = check(
        r#"
        process a() { }
        process b() { a(); }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("use spawn"), "{}", err.message);
}

#[test]
fn test_self_call_as_expression_rejected() {
    let err = check(
        r#"
        process p(int n) { int x = p(n); }
        void main() { }
        "#,
    )
    .unwrap_err();
    assert!(err.message.contains("is a process"), "{}", err.message);
}
