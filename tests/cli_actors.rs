//! End-to-end CLI tests for the MiniC binary on actor programs (`--check` and `--run`).

use std::process::Command;

fn run_cli(flag: &str, fixture: &str) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_mini_c");
    let path = format!("tests/fixtures/{}", fixture);
    let out = Command::new(bin)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([flag, &path])
        .output()
        .expect("failed to execute mini_c binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn test_cli_check_actor_counter() {
    let (code, stdout, stderr) = run_cli("--check", "actor_counter.minic");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout.trim(), "'tests/fixtures/actor_counter.minic' is well-typed.");
}

#[test]
fn test_cli_check_bad_message_exits_1() {
    let (code, _stdout, stderr) = run_cli("--check", "actor_bad_message.minic");
    assert_eq!(code, 1);
    assert!(stderr.contains("never received"), "stderr: {}", stderr);
}

#[test]
fn test_cli_run_actor_counter_prints_7() {
    let (code, stdout, stderr) = run_cli("--run", "actor_counter.minic");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout, "7\n");
}

#[test]
fn test_cli_run_actor_pingpong_deterministic_output() {
    let (code, stdout, stderr) = run_cli("--run", "actor_pingpong.minic");
    assert_eq!(code, 0, "stderr: {}", stderr);
    let expected = "pong\n0\nping\n0\npong\n1\nping\n1\npong\n2\nping\n2\n";
    assert_eq!(stdout, expected);
}

#[test]
fn test_cli_run_actor_isolation_and_pid_display() {
    let (code, stdout, stderr) = run_cli("--run", "actor_isolation.minic");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout, "<pid 0>\n<pid 1>\n99\n10\n");
}

#[test]
fn test_cli_run_actor_deadlock_exits_1() {
    let (code, _stdout, stderr) = run_cli("--run", "actor_deadlock.minic");
    assert_eq!(code, 1);
    assert!(stderr.contains("deadlock"), "stderr: {}", stderr);
}

#[test]
fn test_cli_run_actor_counter_recursive_prints_7() {
    let (code, stdout, stderr) = run_cli("--run", "actor_counter_recursive.minic");
    assert_eq!(code, 0, "stderr: {}", stderr);
    assert_eq!(stdout, "7\n");
}
