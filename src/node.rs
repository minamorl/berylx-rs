//! The workflow itself: named tasks and the combinators that join them.

use std::fmt;
use std::sync::Arc;

use crate::error::Error;
use crate::merge::Conflict;
use crate::outcome::Outcome;

type Run<S> = Arc<dyn Fn(S) -> Outcome<S> + Send + Sync>;
type Predicate<S> = Arc<dyn Fn(&S) -> bool + Send + Sync>;
type Recover<S> = Arc<dyn Fn(S, Error) -> Outcome<S> + Send + Sync>;
type Merger<S> = Arc<dyn Fn(&S, S, S) -> Result<S, Conflict> + Send + Sync>;

/// How a parallel node reacts when one of its branches fails.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OnErr {
    /// Give up at the first failing branch. This is the default, and matches
    /// how a sequence behaves.
    #[default]
    ShortCircuit,
    /// Run every branch, then report all failures together through
    /// [`Error::parallel_errors`].
    Accumulate,
}

/// Joining two states that were produced in parallel.
///
/// Parallel branches each start from a clone of the same state, so something
/// has to decide what the combined state is. In a dynamically typed berylx that
/// decision can default to a deep merge of two hashes; over your own `S` there
/// is no such default that does not silently drop one branch's work, so the
/// choice is yours to state.
///
/// The join sees the state every branch started from as well as the two
/// results. Without that base, a branch that did nothing and a branch that put
/// a field back to its old value look the same, and an independent update to
/// an existing field is silently lost. When both sides changed the same thing
/// in ways that cannot both stand, return a [`Conflict`] rather than picking
/// one: the parallel node then fails with the code `"merge_conflict"` and the
/// base state.
///
/// Implement this to use [`Node::par`], or pass a closure (or one of
/// [`keep_left`](crate::keep_left) and [`keep_right`](crate::keep_right)) to
/// [`Node::par_with`] and skip the trait.
pub trait Merge: Sized {
    /// Combines the state from an earlier branch with a later one, both of
    /// which started from `base`.
    ///
    /// Berylx applies this as a left fold in the order the branches were
    /// written, never in the order they happened to finish, and passes the
    /// same `base` to every call.
    ///
    /// # Errors
    ///
    /// Returns a [`Conflict`] when `left` and `right` changed the state in
    /// ways that cannot both be kept.
    fn merge(base: &Self, left: Self, right: Self) -> Result<Self, Conflict>;
}

/// A named unit of work: one state in, one outcome out.
#[derive(Clone)]
pub struct Task<S> {
    name: String,
    run: Run<S>,
}

impl<S> Task<S> {
    /// Names a function and makes it composable.
    pub fn new(
        name: impl Into<String>,
        run: impl Fn(S) -> Outcome<S> + Send + Sync + 'static,
    ) -> Self {
        Self { name: name.into(), run: Arc::new(run) }
    }

    /// The name this task reports in traces and graphs.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Applies the task to a state.
    #[must_use]
    pub fn call(&self, state: S) -> Outcome<S> {
        (self.run)(state).prepend_trace(self.name.clone())
    }
}

impl<S> fmt::Debug for Task<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task").field("name", &self.name).finish_non_exhaustive()
    }
}

/// One branch of a [`Node::Branch`]: a labelled predicate and what to run.
#[derive(Clone)]
pub struct Arm<S> {
    label: String,
    predicate: Predicate<S>,
    body: Node<S>,
}

impl<S> Arm<S> {
    /// The label this arm shows in a graph.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// What this arm runs when its predicate holds.
    #[must_use]
    pub fn body(&self) -> &Node<S> {
        &self.body
    }

    /// Tests the arm's predicate against a state.
    #[must_use]
    pub fn matches(&self, state: &S) -> bool {
        (self.predicate)(state)
    }
}

impl<S> fmt::Debug for Arm<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm")
            .field("label", &self.label)
            .field("body", &self.body)
            .finish_non_exhaustive()
    }
}

/// A composed workflow, as an inspectable value.
///
/// A `Node` is not a closure that hides what it will do. It is the tree of
/// what the workflow *is*, which is why it can be walked by an alternative
/// [`Interpreter`](crate::Interpreter) to dry-run or audit it, and rendered as
/// a graph, all without running any of it.
#[derive(Clone)]
pub enum Node<S> {
    /// A single named task.
    Task(Task<S>),
    /// Steps run in order, stopping at the first failure.
    Seq(Vec<Node<S>>),
    /// Branches run from the same starting state, then merged.
    Par {
        /// The branches, in the order they were written.
        branches: Vec<Node<S>>,
        /// How two branch states become one.
        merge: Merger<S>,
        /// What to do when a branch fails.
        on_err: OnErr,
    },
    /// The first arm whose predicate holds, or `otherwise`.
    Branch {
        /// The arms, tested in order.
        arms: Vec<Arm<S>>,
        /// What to run when no arm matches. Without it, a non-match is a no-op.
        otherwise: Option<Box<Node<S>>>,
    },
    /// A recovery boundary around `body`.
    Rescue {
        /// The name this boundary reports in traces and graphs.
        name: String,
        /// The protected workflow.
        body: Box<Node<S>>,
        /// Turns a failure and the state it reached back into an outcome.
        recover: Recover<S>,
    },
}

impl<S> fmt::Debug for Node<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Node::Task(task) => f.debug_tuple("Task").field(task).finish(),
            Node::Seq(steps) => f.debug_tuple("Seq").field(steps).finish(),
            Node::Par { branches, on_err, .. } => f
                .debug_struct("Par")
                .field("branches", branches)
                .field("on_err", on_err)
                .finish_non_exhaustive(),
            Node::Branch { arms, otherwise } => {
                f.debug_struct("Branch").field("arms", arms).field("otherwise", otherwise).finish()
            }
            Node::Rescue { name, body, .. } => f
                .debug_struct("Rescue")
                .field("name", name)
                .field("body", body)
                .finish_non_exhaustive(),
        }
    }
}

impl<S> Node<S> {
    /// Runs `next` after `self`, skipping it if `self` failed.
    ///
    /// Sequences flatten: `a.then(b).then(c)` and `a.then(b.then(c))` build the
    /// same three-step node, which is what makes composition associative.
    #[must_use]
    pub fn then(self, next: Node<S>) -> Node<S> {
        let mut steps = match self {
            Node::Seq(steps) => steps,
            node => vec![node],
        };
        match next {
            Node::Seq(rest) => steps.extend(rest),
            node => steps.push(node),
        }
        Node::Seq(steps)
    }

    /// Runs `other` alongside `self`, merging with `merge`.
    ///
    /// Parallel nodes flatten the same way sequences do, so `a.par_with(b, m)`
    /// followed by `.par_with(c, m)` is one three-branch node, not a nest.
    #[must_use]
    pub fn par_with(
        self,
        other: Node<S>,
        merge: impl Fn(&S, S, S) -> Result<S, Conflict> + Send + Sync + 'static,
    ) -> Node<S> {
        self.par_inner(other, Arc::new(merge))
    }

    fn par_inner(self, other: Node<S>, merge: Merger<S>) -> Node<S> {
        let (mut branches, on_err) = match self {
            Node::Par { branches, on_err, .. } => (branches, on_err),
            node => (vec![node], OnErr::default()),
        };
        match other {
            Node::Par { branches: rest, .. } => branches.extend(rest),
            node => branches.push(node),
        }
        Node::Par { branches, merge, on_err }
    }

    /// Catches a failure from `self` and gives it a chance to recover.
    ///
    /// The handler receives the state the failed run actually reached, so
    /// compensation is written against real data rather than a guess.
    #[must_use]
    pub fn rescue_with(
        self,
        name: impl Into<String>,
        recover: impl Fn(S, Error) -> Outcome<S> + Send + Sync + 'static,
    ) -> Node<S> {
        Node::Rescue { name: name.into(), body: Box::new(self), recover: Arc::new(recover) }
    }

    /// Switches a parallel node to [`OnErr::Accumulate`]. A no-op elsewhere.
    #[must_use]
    pub fn accumulate(self) -> Node<S> {
        match self {
            Node::Par { branches, merge, .. } => {
                Node::Par { branches, merge, on_err: OnErr::Accumulate }
            }
            node => node,
        }
    }

    /// Switches a parallel node to [`OnErr::ShortCircuit`]. A no-op elsewhere.
    #[must_use]
    pub fn short_circuit(self) -> Node<S> {
        match self {
            Node::Par { branches, merge, .. } => {
                Node::Par { branches, merge, on_err: OnErr::ShortCircuit }
            }
            node => node,
        }
    }

    /// The name this node reports in traces and graphs.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Node::Task(task) => task.name(),
            Node::Seq(_) => "seq",
            Node::Par { .. } => "par",
            Node::Branch { .. } => "branch",
            Node::Rescue { name, .. } => name,
        }
    }

    /// Every task in this workflow, in written order.
    #[must_use]
    pub fn tasks(&self) -> Vec<&Task<S>> {
        let mut found = Vec::new();
        self.collect_tasks(&mut found);
        found
    }

    fn collect_tasks<'a>(&'a self, found: &mut Vec<&'a Task<S>>) {
        match self {
            Node::Task(task) => found.push(task),
            Node::Seq(steps) => {
                for step in steps {
                    step.collect_tasks(found);
                }
            }
            Node::Par { branches, .. } => {
                for branch in branches {
                    branch.collect_tasks(found);
                }
            }
            Node::Branch { arms, otherwise } => {
                for arm in arms {
                    arm.body.collect_tasks(found);
                }
                if let Some(node) = otherwise {
                    node.collect_tasks(found);
                }
            }
            Node::Rescue { body, .. } => body.collect_tasks(found),
        }
    }
}

impl<S: Merge + 'static> Node<S> {
    /// Runs `other` alongside `self`, merging through the [`Merge`] impl.
    #[must_use]
    pub fn par(self, other: Node<S>) -> Node<S> {
        self.par_inner(other, Arc::new(S::merge))
    }
}

/// Names a function and makes it composable. Shorthand for [`Task::new`].
pub fn task<S>(
    name: impl Into<String>,
    run: impl Fn(S) -> Outcome<S> + Send + Sync + 'static,
) -> Node<S> {
    Node::Task(Task::new(name, run))
}

/// Starts a conditional. Add arms with [`Branch::when`] and close with
/// [`Branch::otherwise`] or [`Branch::end`].
///
/// ```
/// use berylx::{branch, task, Outcome};
///
/// let workflow = branch()
///     .when("empty", |cart: &Vec<u8>| cart.is_empty(), task("reject", Outcome::Ok))
///     .otherwise(task("checkout", Outcome::Ok));
/// ```
#[must_use]
pub fn branch<S>() -> Branch<S> {
    Branch { arms: Vec::new() }
}

/// A conditional under construction.
pub struct Branch<S> {
    arms: Vec<Arm<S>>,
}

impl<S> Branch<S> {
    /// Adds an arm. Arms are tested in the order they were added.
    #[must_use]
    pub fn when(
        mut self,
        label: impl Into<String>,
        predicate: impl Fn(&S) -> bool + Send + Sync + 'static,
        body: Node<S>,
    ) -> Self {
        self.arms.push(Arm { label: label.into(), predicate: Arc::new(predicate), body });
        self
    }

    /// Closes the conditional with a fallback.
    #[must_use]
    pub fn otherwise(self, body: Node<S>) -> Node<S> {
        Node::Branch { arms: self.arms, otherwise: Some(Box::new(body)) }
    }

    /// Closes the conditional without a fallback, making a non-match a no-op.
    #[must_use]
    pub fn end(self) -> Node<S> {
        Node::Branch { arms: self.arms, otherwise: None }
    }
}
