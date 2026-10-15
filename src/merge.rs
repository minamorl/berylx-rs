use std::error::Error as StdError;
use std::fmt;

/// Two parallel branches changed the same part of the state in ways that
/// cannot both stand.
///
/// A reducer returns this instead of quietly keeping one side. The parallel
/// node then fails with the code `"merge_conflict"`, carrying the state every
/// branch started from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    message: String,
}

impl Conflict {
    /// Describes what conflicted. This becomes the failure's message.
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }

    /// What conflicted.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for Conflict {}

/// Joins two branches by keeping the earlier branch's state.
///
/// It works for any `S`, but it drops whatever the later branch did, so it is
/// never chosen for you: pass it to [`Node::par_with`](crate::Node::par_with)
/// when that is what you mean.
///
/// ```
/// use berylx::{keep_left, run, task, Outcome};
///
/// let workflow = task("a", |n: i32| Outcome::Ok(n + 1))
///     .par_with(task("b", |n: i32| Outcome::Ok(n + 10)), keep_left);
///
/// assert_eq!(run(&workflow, 0).ok(), Some(1));
/// ```
///
/// # Errors
///
/// Never returns a [`Conflict`].
pub fn keep_left<S>(_base: &S, left: S, _right: S) -> Result<S, Conflict> {
    Ok(left)
}

/// Joins two branches by keeping the later branch's state.
///
/// It works for any `S`, but it drops whatever the earlier branch did, so it
/// is never chosen for you: pass it to
/// [`Node::par_with`](crate::Node::par_with) when that is what you mean.
///
/// ```
/// use berylx::{keep_right, run, task, Outcome};
///
/// let workflow = task("a", |n: i32| Outcome::Ok(n + 1))
///     .par_with(task("b", |n: i32| Outcome::Ok(n + 10)), keep_right);
///
/// assert_eq!(run(&workflow, 0).ok(), Some(10));
/// ```
///
/// # Errors
///
/// Never returns a [`Conflict`].
pub fn keep_right<S>(_base: &S, _left: S, right: S) -> Result<S, Conflict> {
    Ok(right)
}
