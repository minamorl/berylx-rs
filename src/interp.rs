//! Running a workflow, and wrapping how it runs.

use std::sync::Mutex;

use crate::error::Error;
use crate::node::{Node, OnErr, Task};
use crate::outcome::Outcome;

/// Decides what actually happens when the walk reaches a task.
///
/// The workflow tree says *what* the work is; an `Interpreter` says what
/// running it means. Because the two are separate, retry, dry-run and audit are
/// added by wrapping the interpreter, never by editing the workflow.
pub trait Interpreter<S> {
    /// Handles one task. [`Real`] calls it; others may not.
    fn run_task(&self, task: &Task<S>, state: S) -> Outcome<S>;
}

/// The interpreter that actually performs the work.
#[derive(Clone, Copy, Debug, Default)]
pub struct Real;

impl<S> Interpreter<S> for Real {
    fn run_task(&self, task: &Task<S>, state: S) -> Outcome<S> {
        task.call(state)
    }
}

/// Walks the workflow without performing any task, recording what it would run.
///
/// Predicates are still evaluated, because which arm a branch takes is part of
/// the plan. Task bodies are not, so a dry run touches nothing.
#[derive(Debug, Default)]
pub struct DryRun {
    planned: Mutex<Vec<String>>,
}

impl DryRun {
    /// Creates an empty plan recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The tasks the walk reached, in the order it reached them.
    ///
    /// # Panics
    ///
    /// Panics if a previous walk panicked while recording.
    #[must_use]
    pub fn planned(&self) -> Vec<String> {
        self.planned.lock().expect("dry-run plan poisoned").clone()
    }
}

impl<S> Interpreter<S> for DryRun {
    fn run_task(&self, task: &Task<S>, state: S) -> Outcome<S> {
        self.planned.lock().expect("dry-run plan poisoned").push(task.name().to_owned());
        Outcome::Ok(state)
    }
}

/// Records every task an inner interpreter runs, then delegates to it.
#[derive(Debug, Default)]
pub struct Audit<I> {
    inner: I,
    seen: Mutex<Vec<String>>,
}

impl<I> Audit<I> {
    /// Wraps `inner` so that every task it runs is recorded.
    pub fn new(inner: I) -> Self {
        Self { inner, seen: Mutex::new(Vec::new()) }
    }

    /// The tasks that were run, in the order they were run.
    ///
    /// # Panics
    ///
    /// Panics if a previous walk panicked while recording.
    #[must_use]
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("audit log poisoned").clone()
    }
}

impl<S, I: Interpreter<S>> Interpreter<S> for Audit<I> {
    fn run_task(&self, task: &Task<S>, state: S) -> Outcome<S> {
        self.seen.lock().expect("audit log poisoned").push(task.name().to_owned());
        self.inner.run_task(task, state)
    }
}

/// Retries a failing task, handing each attempt the state the last one reached.
#[derive(Clone, Copy, Debug)]
pub struct Retry<I> {
    inner: I,
    attempts: u32,
}

impl<I> Retry<I> {
    /// Wraps `inner` so each task is tried up to `attempts` times.
    ///
    /// `attempts` counts the first try, so `1` retries nothing and `0` is
    /// treated as `1`.
    pub fn new(inner: I, attempts: u32) -> Self {
        Self { inner, attempts: attempts.max(1) }
    }
}

impl<S, I: Interpreter<S>> Interpreter<S> for Retry<I> {
    fn run_task(&self, task: &Task<S>, state: S) -> Outcome<S> {
        let mut outcome = self.inner.run_task(task, state);
        for _ in 1..self.attempts {
            match outcome {
                ok @ Outcome::Ok(_) => return ok,
                Outcome::Err(reached, _) => outcome = self.inner.run_task(task, reached),
            }
        }
        outcome
    }
}

/// Runs a workflow for real.
///
/// ```
/// use berylx::{run, task, Outcome};
///
/// let workflow = task("double", |n: i32| Outcome::Ok(n * 2))
///     .then(task("inc", |n: i32| Outcome::Ok(n + 1)));
///
/// assert_eq!(run(&workflow, 20).ok(), Some(41));
/// ```
pub fn run<S: Clone + Send>(node: &Node<S>, state: S) -> Outcome<S> {
    run_with(node, state, &Real)
}

/// Runs a workflow under a chosen interpreter.
///
/// ```
/// use berylx::{run_with, task, DryRun, Outcome};
///
/// let workflow = task("charge", |n: i32| Outcome::Ok(n + 1))
///     .then(task("ship", |n: i32| Outcome::Ok(n + 1)));
///
/// let plan = DryRun::new();
/// let outcome = run_with(&workflow, 0, &plan);
///
/// assert_eq!(plan.planned(), vec!["charge", "ship"]);
/// assert_eq!(outcome.ok(), Some(0)); // nothing actually ran
/// ```
pub fn run_with<S, I>(node: &Node<S>, state: S, interp: &I) -> Outcome<S>
where
    S: Clone + Send,
    I: Interpreter<S> + Sync,
{
    match node {
        Node::Task(task) => interp.run_task(task, state),
        Node::Seq(steps) => run_seq(steps, state, interp),
        Node::Par { branches, merge, on_err } => {
            run_par(branches, state, merge.as_ref(), *on_err, interp)
        }
        Node::Branch { arms, otherwise } => {
            for arm in arms {
                if arm.matches(&state) {
                    return run_with(arm.body(), state, interp)
                        .prepend_trace(arm.label().to_owned());
                }
            }
            match otherwise {
                Some(fallback) => run_with(fallback, state, interp).prepend_trace("otherwise"),
                None => Outcome::Ok(state),
            }
        }
        Node::Rescue { name, body, recover } => match run_with(body, state, interp) {
            ok @ Outcome::Ok(_) => ok,
            Outcome::Err(reached, error) => recover(reached, error).prepend_trace(name.clone()),
        },
    }
}

fn run_seq<S, I>(steps: &[Node<S>], state: S, interp: &I) -> Outcome<S>
where
    S: Clone + Send,
    I: Interpreter<S> + Sync,
{
    let mut current = state;
    for step in steps {
        match run_with(step, current, interp) {
            Outcome::Ok(next) => current = next,
            failed @ Outcome::Err(_, _) => return failed,
        }
    }
    Outcome::Ok(current)
}

fn run_par<S, I>(
    branches: &[Node<S>],
    state: S,
    merge: &(dyn Fn(S, S) -> S + Send + Sync),
    on_err: OnErr,
    interp: &I,
) -> Outcome<S>
where
    S: Clone + Send,
    I: Interpreter<S> + Sync,
{
    if branches.is_empty() {
        return Outcome::Ok(state);
    }

    // Branches run at the same time, but their results are folded in the order
    // they were written. Wall-clock order must not reach the state.
    let outcomes: Vec<Outcome<S>> = std::thread::scope(|scope| {
        let handles: Vec<_> = branches
            .iter()
            .map(|branch| {
                let start = state.clone();
                scope.spawn(move || run_with(branch, start, interp))
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("branch panicked")).collect()
    });

    match on_err {
        OnErr::ShortCircuit => fold_short_circuit(outcomes, merge),
        OnErr::Accumulate => fold_accumulate(outcomes, merge),
    }
}

fn fold_short_circuit<S>(
    outcomes: Vec<Outcome<S>>,
    merge: &(dyn Fn(S, S) -> S + Send + Sync),
) -> Outcome<S> {
    let mut merged: Option<S> = None;
    for outcome in outcomes {
        match outcome {
            Outcome::Ok(state) => {
                merged = Some(match merged {
                    Some(left) => merge(left, state),
                    None => state,
                });
            }
            // The first failure in written order wins, and it keeps the state
            // its own branch reached.
            failed @ Outcome::Err(_, _) => return failed,
        }
    }
    Outcome::Ok(merged.expect("parallel node had no branches"))
}

fn fold_accumulate<S>(
    outcomes: Vec<Outcome<S>>,
    merge: &(dyn Fn(S, S) -> S + Send + Sync),
) -> Outcome<S> {
    let mut merged: Option<S> = None;
    let mut errors = Vec::new();

    // Every branch contributes its state, including the ones that failed. That
    // is the point of accumulating: nothing that happened is thrown away.
    for outcome in outcomes {
        let (state, error) = match outcome {
            Outcome::Ok(state) => (state, None),
            Outcome::Err(state, error) => (state, Some(error)),
        };
        merged = Some(match merged {
            Some(left) => merge(left, state),
            None => state,
        });
        if let Some(error) = error {
            errors.push(error);
        }
    }

    let state = merged.expect("parallel node had no branches");
    if errors.is_empty() {
        return Outcome::Ok(state);
    }

    let summary = Error::new("parallel_failed", format!("{} branch(es) failed", errors.len()))
        .with_parallel_errors(errors);
    Outcome::Err(state, summary)
}
