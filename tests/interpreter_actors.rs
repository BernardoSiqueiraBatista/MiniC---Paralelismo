//! Integration tests for the concurrent actor interpreter (milestone 2).

use std::path::Path;

use mini_c::{interpreter::interpret, parser::program, semantic::type_check};
use nom::combinator::all_consuming;

fn run(src: &str) -> Result<(), String> {
    let (_, unchecked) =
        all_consuming(program)(src.trim()).map_err(|e| format!("parse error: {:?}", e))?;
    let checked = type_check(&unchecked).map_err(|e| format!("type error: {}", e.message))?;
    interpret(&checked).map_err(|e| format!("runtime error: {}", e.message))
}

fn run_fixture(name: &str) -> Result<(), String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let src = std::fs::read_to_string(&path).expect("fixture file should exist");
    run(&src)
}

#[test]
fn test_run_fixture_actor_counter() {
    let res = run_fixture("actor_counter.minic");
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_run_fixture_actor_pingpong() {
    let res = run_fixture("actor_pingpong.minic");
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_selective_receive_preserves_unmatched_messages() {
    // Send `second(10)` BEFORE `first(20)`.
    // The first `receive` only accepts `first`, so `second(10)` must stay in
    // the mailbox and be consumed by the subsequent `receive`.
    let src = r#"
        void assert_true(bool ok) {
            if !ok {
                int[] fail = [0];
                int boom = fail[99];
            }
        }

        void main() {
            send self ! second(10);
            send self ! first(20);

            int a = 0;
            int b = 0;
            receive {
                first(int x) => { a = x; }
            }
            receive {
                second(int y) => { b = y; }
            }
            assert_true(a == 20 and b == 10);
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_unmatched_message_does_not_break_later_wakeup() {
    // Worker blocks waiting ONLY for `go(int)`. Main first sends `later(5)`
    // (which does not match `go`), then sends `go(37)`. Worker must wake on
    // `go(37)` and still find `later(5)` in its mailbox afterward.
    let src = r#"
        process worker(pid reply_to) {
            int total = 0;
            receive {
                go(int a) => { total = total + a; }
            }
            receive {
                later(int b) => { total = total + b; }
            }
            send reply_to ! result(total);
        }

        void main() {
            pid w = spawn worker(self);
            send w ! later(5);
            send w ! go(37);
            receive {
                result(int sum) => {
                    if sum != 42 {
                        int[] fail = [0];
                        int boom = fail[99];
                    }
                }
            }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_idle_server_terminates_silently_after_main_finishes() {
    // Counter never receives `stop()`; once `main` exits and quiescence is
    // reached, the idle server must shut down cleanly without error.
    let src = r#"
        process server(int init_val) {
            int state = init_val;
            while true {
                receive {
                    add(int k) => { state = state + k; }
                    query(pid from) => { send from ! reply(state); }
                }
            }
        }

        void main() {
            pid s = spawn server(10);
            send s ! add(32);
            send s ! query(self);
            receive {
                reply(int v) => {
                    if v != 42 {
                        int[] fail = [0];
                        int boom = fail[99];
                    }
                }
            }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_deadlock_in_main_detected() {
    let src = r#"
        void main() {
            receive {
                never_sent(int x) => { }
            }
        }
    "#;
    let err = run(src).unwrap_err();
    assert!(err.contains("deadlock"), "{}", err);
}

#[test]
fn test_mutual_deadlock_between_main_and_worker_detected() {
    let src = r#"
        process worker(pid parent) {
            receive {
                start() => { send parent ! finished(); }
            }
        }

        void main() {
            pid w = spawn worker(self);
            receive {
                finished() => { }
            }
        }
    "#;
    let err = run(src).unwrap_err();
    assert!(err.contains("deadlock"), "{}", err);
}

#[test]
fn test_child_runtime_error_returns_failure_exit() {
    // Child crashes with division by zero while main finishes normally.
    // Overall execution must return Err (so CLI exits with status 1).
    let src = r#"
        process crasher(int zero) {
            int x = 10 / zero;
        }

        void main() {
            pid c = spawn crasher(0);
        }
    "#;
    let err = run(src).unwrap_err();
    assert!(err.contains("terminated with errors"), "{}", err);
}

#[test]
fn test_child_crash_unblocks_waiting_main_via_quiescence() {
    // Child crashes before sending a reply to main; main must not hang forever.
    let src = r#"
        process broken(pid from) {
            int[] a = [1];
            int bad = a[10];
            send from ! ok(bad);
        }

        void main() {
            pid b = spawn broken(self);
            receive {
                ok(int v) => { }
            }
        }
    "#;
    let err = run(src).unwrap_err();
    assert!(err.contains("deadlock"), "{}", err);
}

#[test]
fn test_send_to_terminated_process_is_discarded() {
    let src = r#"
        process short_lived(pid parent) {
            send parent ! done_init();
        }

        void main() {
            pid p = spawn short_lived(self);
            receive {
                done_init() => { }
            }
            send p ! ignored_after_exit(123);
        }
    "#;
    // Need at least one receiver for `ignored_after_exit` so static protocol check passes.
    let src_typed = format!(
        "{}\nprocess dummy() {{ receive {{ ignored_after_exit(int x) => {{ }} }} }}",
        src
    );
    let res = run(&src_typed);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_memory_isolation_arrays_copied_on_spawn_and_send() {
    let src = r#"
        process mutator(int[] spawned_arr, pid reply_to) {
            spawned_arr[0] = 777;
            receive {
                payload(int[] msg_arr) => {
                    msg_arr[1] = 888;
                    send reply_to ! ack(spawned_arr[0], msg_arr[1]);
                }
            }
        }

        void main() {
            int[] a = [1, 2, 3];
            pid m = spawn mutator(a, self);
            send m ! payload(a);
            receive {
                ack(int child0, int child1) => {
                    if child0 != 777 or child1 != 888 {
                        int[] f = [0];
                        int b = f[99];
                    }
                }
            }
            if a[0] != 1 or a[1] != 2 or a[2] != 3 {
                int[] f = [0];
                int b = f[99];
            }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_self_inside_helper_function_and_pid_equality() {
    let src = r#"
        pid who_am_i() {
            return self;
        }

        process echo_pid(pid parent) {
            send parent ! my_pid(who_am_i());
        }

        void main() {
            pid main_id = who_am_i();
            if main_id != self {
                int[] f = [0];
                int b = f[99];
            }
            pid child = spawn echo_pid(self);
            receive {
                my_pid(pid reported) => {
                    if reported != child or reported == self {
                        int[] f = [0];
                        int b = f[99];
                    }
                }
            }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_parallel_workers_sum_of_squares() {
    // Spawn 50 concurrent workers computing i*i in parallel and sending back to main.
    // Expected sum_{i=1}^{50} i^2 = 50 * 51 * 101 / 6 = 42925.
    let src = r#"
        process sq_worker(pid collector, int x) {
            send collector ! partial(x * x);
        }

        void main() {
            int n = 50;
            int i = 1;
            while i <= n {
                pid w = spawn sq_worker(self, i);
                i = i + 1;
            }

            int received = 0;
            int sum = 0;
            while received < n {
                receive {
                    partial(int val) => { sum = sum + val; }
                }
                received = received + 1;
            }

            if sum != 42925 {
                int[] f = [0];
                int b = f[99];
            }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

// --- Tail-recursive processes (Erlang-style server loop) ---

#[test]
fn test_run_fixture_actor_counter_recursive() {
    let res = run_fixture("actor_counter_recursive.minic");
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_recursive_process_keeps_pid_and_mailbox() {
    // The process restarts but keeps its pid (`self`) and its mailbox: the
    // three `ping`s are all sent before it restarts the first time.
    let src = r#"
        process echo(int round, pid parent) {
            receive {
                ping() => {
                    send parent ! pong(self, round);
                    echo(round + 1, parent);
                }
                stop() => { }
            }
        }
        void main() {
            pid e = spawn echo(0, self);
            send e ! ping();
            send e ! ping();
            send e ! ping();
            int got = 0;
            while got < 3 {
                receive {
                    pong(pid from, int round) => {
                        if from != e or round != got {
                            int[] f = [0];
                            int b = f[99];
                        }
                        got = got + 1;
                    }
                }
            }
            send e ! stop();
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_recursive_process_does_not_grow_stack() {
    // 200_000 restarts would overflow the stack if each self-call nested a
    // Rust call; the runtime restarts the body in a loop instead.
    let src = r#"
        process countdown(int n, pid parent) {
            if n == 0 {
                send parent ! finished();
            } else {
                countdown(n - 1, parent);
            }
        }
        void main() {
            pid c = spawn countdown(200000, self);
            receive { finished() => { print("ok"); } }
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}

#[test]
fn test_idle_recursive_server_shuts_down_when_main_ends() {
    let src = r#"
        process server(int n) {
            receive {
                inc() => { server(n + 1); }
            }
        }
        void main() {
            pid s = spawn server(0);
            send s ! inc();
        }
    "#;
    let res = run(src);
    assert!(res.is_ok(), "{:?}", res.err());
}
