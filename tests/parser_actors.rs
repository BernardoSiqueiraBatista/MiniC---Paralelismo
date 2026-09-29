//! Parser tests for the actor extension (milestone 1: syntax, AST and parser only).

use mini_c::ir::ast::{
    Expr, ExprD, Literal, Message, ReceiveClause, Statement, StatementD, Type, UncheckedExpr,
    UncheckedProgram,
};
use mini_c::parser::{expression, process_decl, program, statement};
use nom::combinator::all_consuming;
use std::path::Path;

fn ident(name: &str) -> UncheckedExpr {
    ExprD { exp: Expr::Ident(name.to_string()), ty: () }
}

fn int(n: i64) -> UncheckedExpr {
    ExprD { exp: Expr::Literal(Literal::Int(n)), ty: () }
}

fn parse_program_file(name: &str) -> UncheckedProgram {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let src = std::fs::read_to_string(&path).expect("fixture file should exist");
    let (_, prog) = all_consuming(program)(src.trim()).expect("fixture should parse");
    prog
}

// --- process declarations ---

#[test]
fn test_process_with_params() {
    let (rest, p) = process_decl("process worker(int id, pid parent) { print(id); }").unwrap();
    assert_eq!(rest, "");
    assert_eq!(p.name, "worker");
    assert_eq!(
        p.params,
        vec![("id".to_string(), Type::Int), ("parent".to_string(), Type::Pid)]
    );
    assert!(matches!(p.body.stmt, Statement::Block { ref seq } if seq.len() == 1));
}

#[test]
fn test_process_without_params() {
    let (rest, p) = process_decl("process idle() { }").unwrap();
    assert_eq!(rest, "");
    assert_eq!(p.name, "idle");
    assert!(p.params.is_empty());
    assert_eq!(p.body.stmt, Statement::Block { seq: vec![] });
}

#[test]
fn test_process_pid_array_param() {
    let (_, p) = process_decl("process hub(pid[] peers) { }").unwrap();
    assert_eq!(p.params, vec![("peers".to_string(), Type::Array(Box::new(Type::Pid)))]);
}

#[test]
fn test_process_body_must_be_block() {
    assert!(process_decl("process p() print(1);").is_err());
    assert!(process_decl("process p() x = 1;").is_err());
}

#[test]
fn test_process_has_no_return_type() {
    assert!(process_decl("process int p() { }").is_err());
}

// --- spawn / self ---

#[test]
fn test_spawn_with_args() {
    let (rest, e) = expression("spawn worker(1, x)").unwrap();
    assert_eq!(rest, "");
    assert_eq!(
        e.exp,
        Expr::Spawn { process: "worker".to_string(), args: vec![int(1), ident("x")] }
    );
}

#[test]
fn test_spawn_without_args() {
    let (_, e) = expression("spawn idle()").unwrap();
    assert_eq!(e.exp, Expr::Spawn { process: "idle".to_string(), args: vec![] });
}

#[test]
fn test_spawn_requires_call_syntax() {
    assert!(all_consuming(expression)("spawn idle").is_err());
}

#[test]
fn test_self_expression() {
    let (rest, e) = expression("self").unwrap();
    assert_eq!(rest, "");
    assert_eq!(e.exp, Expr::SelfPid);
}

#[test]
fn test_self_inside_expression() {
    let (_, e) = expression("self == p").unwrap();
    assert_eq!(
        e.exp,
        Expr::Eq(Box::new(ExprD { exp: Expr::SelfPid, ty: () }), Box::new(ident("p")))
    );
}

#[test]
fn test_pid_declaration() {
    let (_, s) = statement("pid w = spawn worker(3);").unwrap();
    match s.stmt {
        Statement::Decl { name, ty, init } => {
            assert_eq!(name, "w");
            assert_eq!(ty, Type::Pid);
            assert!(matches!(init.exp, Expr::Spawn { .. }));
        }
        other => panic!("expected Decl, got {:?}", other),
    }
}

// --- keywords must match whole words ---

#[test]
fn test_keyword_prefixes_are_identifiers() {
    for name in ["spawner", "selfish", "sender", "receiver", "processor", "pids", "self_"] {
        let (rest, e) = expression(name).unwrap();
        assert_eq!(rest, "", "{}", name);
        assert_eq!(e.exp, Expr::Ident(name.to_string()), "{}", name);
    }
}

#[test]
fn test_keyword_prefix_calls_and_assignments() {
    let (_, e) = expression("spawner(1)").unwrap();
    assert_eq!(e.exp, Expr::Call { name: "spawner".to_string(), args: vec![int(1)] });

    let (_, s) = statement("sender = 1;").unwrap();
    assert!(matches!(s.stmt, Statement::Assign { .. }));

    let (_, s) = statement("receiver(2);").unwrap();
    assert!(matches!(s.stmt, Statement::Call { ref name, .. } if name == "receiver"));
}

#[test]
fn test_keywords_are_not_identifiers() {
    for kw in ["process", "spawn", "send", "receive", "pid"] {
        assert!(all_consuming(expression)(kw).is_err(), "{}", kw);
    }
    assert!(statement("int self = 1;").is_err());
    assert!(statement("int spawn = 1;").is_err());
}

// --- send ---

#[test]
fn test_send_statement() {
    let (rest, s) = statement("send p ! inc(1, x);").unwrap();
    assert_eq!(rest, "");
    assert_eq!(
        s.stmt,
        Statement::Send {
            target: Box::new(ident("p")),
            message: Message { tag: "inc".to_string(), args: vec![int(1), ident("x")] },
        }
    );
}

#[test]
fn test_send_without_spaces_and_no_args() {
    let (_, s) = statement("send p!stop();").unwrap();
    assert_eq!(
        s.stmt,
        Statement::Send {
            target: Box::new(ident("p")),
            message: Message { tag: "stop".to_string(), args: vec![] },
        }
    );
}

#[test]
fn test_send_to_expression_target() {
    let (_, s) = statement("send peers[i] ! ping(self);").unwrap();
    match s.stmt {
        Statement::Send { target, message } => {
            assert!(matches!(target.exp, Expr::Index { .. }));
            assert_eq!(message.args, vec![ExprD { exp: Expr::SelfPid, ty: () }]);
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn test_send_vs_not_equal() {
    // `!=` inside the target stays a comparison; the lone `!` is the separator.
    let (rest, s) = statement("send a != b ! m();").unwrap();
    assert_eq!(rest, "");
    match s.stmt {
        Statement::Send { target, message } => {
            assert_eq!(target.exp, Expr::Ne(Box::new(ident("a")), Box::new(ident("b"))));
            assert_eq!(message.tag, "m");
        }
        other => panic!("expected Send, got {:?}", other),
    }
    // `!=` is never accepted as the send separator.
    assert!(statement("send p != m();").is_err());
}

#[test]
fn test_not_equal_still_works() {
    let (_, e) = expression("x != 1").unwrap();
    assert_eq!(e.exp, Expr::Ne(Box::new(ident("x")), Box::new(int(1))));
    let (_, e) = expression("!x").unwrap();
    assert_eq!(e.exp, Expr::Not(Box::new(ident("x"))));
}

#[test]
fn test_send_requires_semicolon_and_message() {
    assert!(statement("send p ! m()").is_err());
    assert!(statement("send p ! m;").is_err());
    assert!(statement("send p m();").is_err());
}

// --- receive ---

#[test]
fn test_receive_multiple_clauses() {
    let src = "receive {
        inc(int k) => { n = n + k; }
        get(pid from) => { send from ! value(n); }
        stop() => { }
    }";
    let (rest, s) = statement(src).unwrap();
    assert_eq!(rest, "");
    let clauses = match s.stmt {
        Statement::Receive { clauses } => clauses,
        other => panic!("expected Receive, got {:?}", other),
    };
    assert_eq!(clauses.len(), 3);
    assert_eq!(clauses[0].tag, "inc");
    assert_eq!(clauses[0].params, vec![("k".to_string(), Type::Int)]);
    assert_eq!(clauses[1].tag, "get");
    assert_eq!(clauses[1].params, vec![("from".to_string(), Type::Pid)]);
    assert!(matches!(
        clauses[1].body.stmt,
        Statement::Block { ref seq } if matches!(seq[0].stmt, Statement::Send { .. })
    ));
    assert_eq!(
        clauses[2],
        ReceiveClause {
            tag: "stop".to_string(),
            params: vec![],
            body: Box::new(StatementD { stmt: Statement::Block { seq: vec![] }, ty: () }),
        }
    );
}

#[test]
fn test_receive_empty_rejected() {
    assert!(statement("receive { }").is_err());
    assert!(statement("receive {}").is_err());
}

#[test]
fn test_receive_clause_body_must_be_block() {
    assert!(statement("receive { m() => print(1); }").is_err());
}

#[test]
fn test_receive_needs_no_semicolon() {
    let (_, s) = statement("{ receive { m() => { } } x = 1; }").unwrap();
    match s.stmt {
        Statement::Block { seq } => {
            assert_eq!(seq.len(), 2);
            assert!(matches!(seq[0].stmt, Statement::Receive { .. }));
            assert!(matches!(seq[1].stmt, Statement::Assign { .. }));
        }
        other => panic!("expected Block, got {:?}", other),
    }
}

// --- whole programs ---

#[test]
fn test_mixed_program_any_order() {
    let src = "process a() { } void main() { pid x = spawn a(); } int f() { return 1; } process b(int n) { }";
    let (rest, prog) = program(src).unwrap();
    assert_eq!(rest, "");
    let fnames: Vec<_> = prog.functions.iter().map(|f| f.name.as_str()).collect();
    let pnames: Vec<_> = prog.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(fnames, vec!["main", "f"]);
    assert_eq!(pnames, vec!["a", "b"]);
}

#[test]
fn test_program_without_processes() {
    let (_, prog) = program("void main() { }").unwrap();
    assert!(prog.processes.is_empty());
}

#[test]
fn test_fixture_actor_counter() {
    let prog = parse_program_file("actor_counter.minic");
    assert_eq!(prog.processes.len(), 1);
    assert_eq!(prog.processes[0].name, "counter");
    assert!(prog.main_function().is_some());
}

#[test]
fn test_fixture_actor_pingpong() {
    let prog = parse_program_file("actor_pingpong.minic");
    let pnames: Vec<_> = prog.processes.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(pnames, vec!["pong", "ping"]);
    assert_eq!(prog.functions.len(), 2);
}

// --- later phases reject the new constructs explicitly ---

#[test]
fn test_type_checker_rejects_processes() {
    let prog = parse_program_file("actor_counter.minic");
    let err = mini_c::semantic::type_check(&prog).unwrap_err();
    assert!(err.message.contains("not yet supported"), "{}", err.message);
}

#[test]
fn test_type_checker_rejects_actor_statements() {
    let (_, prog) = program("void main() { send self ! m(); }").unwrap();
    let err = mini_c::semantic::type_check(&prog).unwrap_err();
    assert!(err.message.contains("not yet supported"), "{}", err.message);
}
