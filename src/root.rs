//! The one owner of committed state.

use crate::interp::{run_with, Interpreter, Real};
use crate::node::Node;
use crate::outcome::Outcome;

/// Owns the committed state for one application boundary.
///
/// A `Root` is the answer to "which state is real?". Workflows run against a
/// clone of it and hand back an outcome; the root advances only when that
/// outcome succeeded. A failed run still returns the state it reached, but that
/// state stays a diagnosis — it never quietly becomes the committed truth.
///
/// ```
/// use berylx::{task, Error, Outcome, Root};
///
/// let mut root = Root::new(0_i32);
/// let failing = task("bump", |n: i32| Outcome::Ok(n + 1))
///     .then(task("boom", |n: i32| Outcome::Err(n, Error::new("boom", "no"))));
///
/// let outcome = root.run(&failing);
///
/// assert_eq!(outcome.state(), &1); // the run got this far
/// assert_eq!(root.state(), &0);    // but nothing was committed
/// ```
#[derive(Clone, Debug)]
pub struct Root<S> {
    state: S,
}

impl<S> Root<S> {
    /// Takes ownership of the starting state.
    pub const fn new(state: S) -> Self {
        Self { state }
    }

    /// Borrows the committed state.
    pub const fn state(&self) -> &S {
        &self.state
    }

    /// Gives up the root and returns the committed state.
    #[must_use]
    pub fn into_state(self) -> S {
        self.state
    }
}

impl<S: Clone + Send> Root<S> {
    /// Runs a workflow, committing only if it succeeded.
    ///
    /// The commit happens once per run, after the whole workflow has
    /// succeeded, never task by task. A failed run leaves the committed state
    /// as it was, but neither a commit nor a failure undoes what the tasks did
    /// outside the state: berylx composes work within one process, and
    /// compensating an external side effect is the job of a
    /// [`Node::rescue_with`] handler.
    ///
    /// To run a workflow without committing anywhere, use [`run`](crate::run).
    pub fn run(&mut self, node: &Node<S>) -> Outcome<S> {
        self.run_with(node, &Real)
    }

    /// Runs a workflow under a chosen interpreter, committing only on success.
    pub fn run_with<I>(&mut self, node: &Node<S>, interp: &I) -> Outcome<S>
    where
        I: Interpreter<S> + Sync,
    {
        let outcome = run_with(node, self.state.clone(), interp);
        if let Outcome::Ok(next) = &outcome {
            self.state = next.clone();
        }
        outcome
    }
}
