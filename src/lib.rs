//! Workflows whose failures carry the state they reached.
//!
//! A multi-step business process fails halfway. Rust's usual answer,
//! `Result<T, E>`, throws away the `T` at that moment — so the code that has to
//! clean up gets an error and nothing else, and has to infer what already
//! happened. Berylx changes exactly one thing:
//!
//! ```text
//! Task : S -> Outcome<S>          where Outcome<S> = Ok(S) | Err(S, Error)
//! ```
//!
//! The failure variant carries the state too, so [`Outcome::state`] is total:
//! there is no result from which the state is missing. Compensation is written
//! against data you have.
//!
//! ```
//! use berylx::{task, Error, Outcome, Root};
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct Checkout {
//!     charged: bool,
//!     refunded: bool,
//!     notified: bool,
//! }
//!
//! let charge = task("charge", |mut c: Checkout| {
//!     c.charged = true;
//!     Outcome::Err(c, Error::new("payment_failed", "card declined"))
//! });
//!
//! let notify = task("notify", |mut c: Checkout| {
//!     c.notified = true;
//!     Outcome::Ok(c)
//! });
//!
//! // The handler sees the state the failure reached: `charged` is already
//! // true here, so it knows a refund is genuinely owed.
//! let workflow = charge
//!     .rescue_with("refund", |mut c: Checkout, _e| {
//!         assert!(c.charged);
//!         c.refunded = true;
//!         Outcome::Ok(c)
//!     })
//!     .then(notify);
//!
//! let mut root = Root::new(Checkout { charged: false, refunded: false, notified: false });
//! let outcome = root.run(&workflow);
//!
//! assert!(outcome.is_ok());
//! assert_eq!(root.state().refunded, true);
//! ```
//!
//! # What the crate is made of
//!
//! - [`Outcome`] — the result envelope, and the whole reason this exists.
//! - [`Error`] — a failure that knows its code, its failing node and its route.
//! - [`Node`] — a composed workflow, as a value you can inspect and render.
//! - [`Root`] — the single owner of committed state.
//! - [`Interpreter`] — what running a task *means*, so retry, dry-run and audit
//!   are added by wrapping rather than by editing the workflow.
//!
//! # Composing
//!
//! Combinators are methods, not operators. Rust's [API guidelines][overload]
//! ask that an overloaded operator share the properties expected of it, and
//! berylx's parallel composition is not commutative, so spelling it `&` would
//! be a promise the crate cannot keep.
//!
//! ```
//! use berylx::{branch, run, task, Conflict, Merge, Outcome};
//!
//! #[derive(Clone, Debug, PartialEq)]
//! struct Counts { a: u32, b: u32 }
//!
//! impl Merge for Counts {
//!     // Both branches started from `base`, so each side's work is its
//!     // difference from it.
//!     fn merge(base: &Self, left: Self, right: Self) -> Result<Self, Conflict> {
//!         Ok(Counts { a: left.a + right.a - base.a, b: left.b + right.b - base.b })
//!     }
//! }
//!
//! let workflow = task("left", |mut c: Counts| { c.a += 1; Outcome::Ok(c) })
//!     .par(task("right", |mut c: Counts| { c.b += 1; Outcome::Ok(c) }))
//!     .then(
//!         branch()
//!             .when("both", |c: &Counts| c.a > 0 && c.b > 0, task("done", Outcome::Ok))
//!             .end(),
//!     );
//!
//! let outcome = run(&workflow, Counts { a: 0, b: 0 });
//! assert_eq!(outcome.ok(), Some(Counts { a: 1, b: 1 }));
//! ```
//!
//! Branches run at the same time, on real threads. Their results are folded in
//! the order they were *written*, never the order they finished, so a workflow
//! that succeeds does so deterministically.
//!
//! [overload]: https://rust-lang.github.io/api-guidelines/predictability.html#c-overload

// Declared here rather than in `[lints]`, which Cargo only understands from
// 1.74 and this crate supports 1.70.
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod error;
mod graph;
mod interp;
mod merge;
mod node;
mod outcome;
mod root;

pub use crate::error::Error;
pub use crate::graph::{to_dot, to_mermaid};
pub use crate::interp::{run, run_with, Audit, Backoff, DryRun, Interpreter, Real, Retry};
pub use crate::merge::{keep_left, keep_right, Conflict};
pub use crate::node::{branch, task, Arm, Branch, Merge, Node, OnErr, Task};
pub use crate::outcome::Outcome;
pub use crate::root::Root;

/// Everything you normally want in scope to write a workflow.
pub mod prelude {
    pub use crate::error::Error;
    pub use crate::interp::{run, run_with, Backoff, DryRun, Interpreter, Real, Retry};
    pub use crate::merge::{keep_left, keep_right, Conflict};
    pub use crate::node::{branch, task, Merge, Node, OnErr};
    pub use crate::outcome::Outcome;
    pub use crate::root::Root;
}
