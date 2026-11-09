//! Concurrent actor runtime for the MiniC interpreter.
//!
//! # Overview
//!
//! Each MiniC process (including `main`, registered as `pid 0`) runs on its own
//! OS thread with an isolated [`Environment<Value>`](crate::environment::Environment)
//! and communicates exclusively through asynchronous message passing.
//!
//! Key types:
//!
//! * [`Runtime`] — shared coordinator holding process mailboxes, lifecycle
//!   counters, and synchronization primitives.
//! * [`ProcCtx`] — per-process execution context (`pid` + `Arc<Runtime>`)
//!   threaded through `exec_stmt` and `eval_expr`.
//! * [`RtMessage`] — a runtime message `(tag, args)` stored in a mailbox.
//!
//! # Eliminating Busy-Waiting (Per-Process Notification)
//!
//! Processes **never** spin or poll their mailboxes. Each process owns a
//! dedicated [`Condvar`] (`ProcEntry::cond`):
//!
//! 1. When a process executes `receive { t1(...) => ..., tk(...) => ... }` and
//!    its mailbox has no matching message, it records its status as
//!    `Status::Waiting(vec![t1, ..., tk])` and suspends its OS thread via
//!    `entry.cond.wait(guard)` (0% CPU usage).
//! 2. When another process executes `send target ! tag(args);`, it pushes the
//!    message into `target`'s mailbox and checks whether `target` is currently
//!    `Status::Waiting(waiting_tags)` with `tag` in `waiting_tags`. **Only**
//!    when a matching message arrives does `send` transition `target` back to
//!    `Status::Running` and call `target.cond.notify_one()` (after releasing
//!    the mutex).
//!
//! This avoids both busy-waiting and "thundering herd" wakeups (`notify_all`),
//! ensuring that unrelated sleeping processes stay asleep and a waiting process
//! is only woken when a message it can actually consume arrives (or on runtime
//! shutdown).
//!
//! # Quiescence and Deadlock Detection
//!
//! Because `waiting` counts processes blocked in `receive` whose mailboxes have
//! zero matching messages, `waiting == alive` (with `alive > 0`) means **no**
//! process in the system can make further progress (quiescence):
//!
//! * If `main` (`MAIN_PID = 0`) is still alive at quiescence, `receive` fails
//!   with a deadlock [`RuntimeError`].
//! * If `main` has already finished, remaining blocked processes are idle
//!   servers (e.g., a counter loop that was never sent `stop()`); they unwind
//!   silently via [`RuntimeError::shutdown`].

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use crate::ir::ast::{CheckedProcessDecl, CheckedProgram};

use super::exec_stmt::exec_stmt;
use super::value::{RuntimeError, Value};

/// Process identifier.
pub type Pid = u64;

/// Reserved PID for the initial `main` process.
pub const MAIN_PID: Pid = 0;

/// Stack size (8 MiB) allocated for spawned actor threads to support recursion.
const ACTOR_STACK_SIZE: usize = 8 * 1024 * 1024;

/// A tagged message stored inside a process mailbox.
#[derive(Debug, Clone)]
pub struct RtMessage {
    pub tag: String,
    pub args: Vec<Value>,
}

/// Lifecycle state of an actor process.
enum Status {
    Running,
    /// Blocked in `receive`, waiting for a message whose tag is in this list.
    Waiting(Vec<String>),
    Finished,
}

struct ProcEntry {
    name: String,
    status: Status,
    mailbox: VecDeque<RtMessage>,
    /// Per-process condition variable: notified via `notify_one` only when a
    /// matching message arrives for this process or when `shutdown` triggers.
    cond: Arc<Condvar>,
}

struct State {
    procs: HashMap<Pid, ProcEntry>,
    next_pid: Pid,
    alive: usize,
    waiting: usize,
    shutdown: bool,
    errors: Vec<RuntimeError>,
    handles: Vec<JoinHandle<()>>,
}

/// Shared runtime coordinating all processes in a MiniC execution.
pub struct Runtime {
    state: Mutex<State>,
    /// Condition variable notified when all processes finish or quiescence occurs.
    done_cond: Condvar,
    pub program: CheckedProgram,
}

/// Execution context for the currently running process.
#[derive(Clone)]
pub struct ProcCtx {
    pub pid: Pid,
    pub rt: Arc<Runtime>,
}

fn trigger_shutdown(st: &mut State, done_cond: &Condvar) -> Vec<Arc<Condvar>> {
    st.shutdown = true;
    let waiters = st
        .procs
        .values()
        .filter(|p| matches!(p.status, Status::Waiting(_)))
        .map(|p| Arc::clone(&p.cond))
        .collect();
    done_cond.notify_all();
    waiters
}

impl Runtime {
    /// Create a new runtime with `main` registered as `MAIN_PID` (`0`).
    pub fn new(program: CheckedProgram) -> Self {
        let mut procs = HashMap::new();
        procs.insert(
            MAIN_PID,
            ProcEntry {
                name: "main".to_string(),
                status: Status::Running,
                mailbox: VecDeque::new(),
                cond: Arc::new(Condvar::new()),
            },
        );
        Self {
            state: Mutex::new(State {
                procs,
                next_pid: 1,
                alive: 1,
                waiting: 0,
                shutdown: false,
                errors: Vec::new(),
                handles: Vec::new(),
            }),
            done_cond: Condvar::new(),
            program,
        }
    }

    /// Spawn a new OS thread executing process `name` with cloned `args`.
    pub fn spawn(self: &Arc<Self>, name: &str, args: Vec<Value>) -> Result<Pid, RuntimeError> {
        let decl = self
            .program
            .processes
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| RuntimeError::new(format!("undefined process '{}'", name)))?;

        let mut st = self.state.lock().unwrap();
        let pid = st.next_pid;
        st.next_pid += 1;
        st.procs.insert(
            pid,
            ProcEntry {
                name: name.to_string(),
                status: Status::Running,
                mailbox: VecDeque::new(),
                cond: Arc::new(Condvar::new()),
            },
        );
        // Increment `alive` before launching the thread so `waiting == alive`
        // cannot fire prematurely before the child begins running.
        st.alive += 1;

        let rt = Arc::clone(self);
        let spawn_res = thread::Builder::new()
            .name(format!("{}#{}", name, pid))
            .stack_size(ACTOR_STACK_SIZE)
            .spawn(move || run_process(rt, pid, decl, args));

        match spawn_res {
            Ok(handle) => {
                st.handles.push(handle);
                Ok(pid)
            }
            Err(e) => {
                st.alive -= 1;
                st.procs.remove(&pid);
                Err(RuntimeError::new(format!(
                    "failed to spawn process '{}': {}",
                    name, e
                )))
            }
        }
    }

    /// Asynchronously enqueue `msg` into `to`'s mailbox.
    ///
    /// If `to` has already finished, the message is silently discarded.
    /// If `to` is blocked in `receive` waiting for `msg.tag`, its status is
    /// atomically switched to `Running` and its dedicated `Condvar` is notified
    /// with `notify_one`.
    pub fn send(&self, to: Pid, msg: RtMessage) {
        let mut guard = self.state.lock().unwrap();
        let st = &mut *guard;
        let wake_cond = if let Some(entry) = st.procs.get_mut(&to) {
            if matches!(entry.status, Status::Finished) {
                return;
            }
            let matches_wait = match &entry.status {
                Status::Waiting(waiting_tags) => waiting_tags.iter().any(|t| t == &msg.tag),
                _ => false,
            };
            entry.mailbox.push_back(msg);
            if matches_wait {
                entry.status = Status::Running;
                st.waiting -= 1;
                Some(Arc::clone(&entry.cond))
            } else {
                None
            }
        } else {
            None
        };
        // Release the mutex before waking the recipient thread so it can
        // immediately acquire the lock upon waking.
        drop(guard);
        if let Some(cond) = wake_cond {
            cond.notify_one();
        }
    }

    /// Selective blocking receive: scan `me`'s mailbox for the oldest message
    /// whose tag is in `tags`, sleeping on `me`'s `Condvar` until one arrives
    /// or quiescence is reached.
    ///
    /// Returns `(clause_index, message_args)`.
    pub fn receive(&self, me: Pid, tags: &[String]) -> Result<(usize, Vec<Value>), RuntimeError> {
        let mut guard = self.state.lock().unwrap();
        loop {
            let st = &mut *guard;
            let entry = st.procs.get_mut(&me).expect("current process must exist");

            if let Some(pos) = entry.mailbox.iter().position(|m| tags.contains(&m.tag)) {
                let msg = entry.mailbox.remove(pos).unwrap();
                if matches!(entry.status, Status::Waiting(_)) {
                    entry.status = Status::Running;
                    st.waiting -= 1;
                }
                let idx = tags.iter().position(|t| *t == msg.tag).unwrap();
                return Ok((idx, msg.args));
            }

            if !st.shutdown {
                match &mut entry.status {
                    Status::Waiting(prev_tags) => {
                        *prev_tags = tags.to_vec();
                    }
                    Status::Running => {
                        entry.status = Status::Waiting(tags.to_vec());
                        st.waiting += 1;
                    }
                    Status::Finished => unreachable!("finished process cannot call receive"),
                }

                if st.waiting == st.alive {
                    let waiters = trigger_shutdown(st, &self.done_cond);
                    let entry = st.procs.get_mut(&me).unwrap();
                    if matches!(entry.status, Status::Waiting(_)) {
                        entry.status = Status::Running;
                        st.waiting -= 1;
                    }
                    drop(guard);
                    for c in waiters {
                        c.notify_one();
                    }
                    return Err(if me == MAIN_PID {
                        RuntimeError::new(
                            "deadlock: main is waiting for a message that will never arrive",
                        )
                    } else {
                        RuntimeError::shutdown()
                    });
                }
            }

            if st.shutdown {
                if matches!(entry.status, Status::Waiting(_)) {
                    entry.status = Status::Running;
                    st.waiting -= 1;
                }
                return Err(if me == MAIN_PID {
                    RuntimeError::new(
                        "deadlock: main is waiting for a message that will never arrive",
                    )
                } else {
                    RuntimeError::shutdown()
                });
            }

            let my_cond = Arc::clone(&entry.cond);
            guard = my_cond.wait(guard).unwrap();
        }
    }

    /// Mark process `me` as finished, record any non-shutdown child error,
    /// and trigger quiescence if all remaining alive processes are waiting.
    pub fn finish(&self, me: Pid, result: Result<(), RuntimeError>) {
        let mut guard = self.state.lock().unwrap();
        let st = &mut *guard;
        let entry = st.procs.get_mut(&me).expect("current process must exist");
        if matches!(entry.status, Status::Waiting(_)) {
            st.waiting -= 1;
        }
        entry.status = Status::Finished;
        entry.mailbox.clear();
        st.alive -= 1;

        if let Err(e) = result {
            if !e.shutdown && me != MAIN_PID {
                eprintln!("RuntimeError in process {}#{}: {}", entry.name, me, e.message);
                st.errors.push(e);
            }
        }

        let waiters = if st.alive > 0 && st.waiting == st.alive && !st.shutdown {
            trigger_shutdown(st, &self.done_cond)
        } else {
            if st.alive == 0 {
                self.done_cond.notify_all();
            }
            Vec::new()
        };
        drop(guard);
        for c in waiters {
            c.notify_one();
        }
    }

    /// Wait until all processes finish (or reach quiescence), join all child
    /// threads, and return `Err` if any child process failed with a runtime error.
    pub fn wait_all(&self) -> Result<(), RuntimeError> {
        let mut guard = self.state.lock().unwrap();
        while guard.alive > 0 && !guard.shutdown {
            guard = self.done_cond.wait(guard).unwrap();
        }
        let handles = std::mem::take(&mut guard.handles);
        // Must release mutex before joining so waking threads can run `finish`.
        drop(guard);
        for h in handles {
            let _ = h.join();
        }
        let st = self.state.lock().unwrap();
        match st.errors.len() {
            0 => Ok(()),
            n => Err(RuntimeError::new(format!(
                "{} process(es) terminated with errors",
                n
            ))),
        }
    }
}

fn run_process(rt: Arc<Runtime>, pid: Pid, decl: CheckedProcessDecl, args: Vec<Value>) {
    let ctx = ProcCtx {
        pid,
        rt: Arc::clone(&rt),
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut env = super::base_env(&rt.program);
        for ((name, _), val) in decl.params.iter().zip(args) {
            env.declare(name.clone(), val);
        }
        exec_stmt(&decl.body, &mut env, &ctx).map(|_| ())
    }));
    let result = match outcome {
        Ok(res) => res,
        Err(_) => Err(RuntimeError::new("internal panic in process")),
    };
    rt.finish(pid, result);
}
