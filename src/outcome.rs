//! The result envelope: a success, or a failure that still holds its state.

use crate::error::Error;

/// The outcome of running a workflow node against a state.
///
/// This is [`Result`] with one deliberate change: the error variant also
/// carries the state. That single difference is what berylx exists for.
///
/// With `Result<S, Error>`, a failure discards `S`. Whatever the workflow had
/// already accomplished — the charge that went through, the row that was
/// inserted — is gone from the type, and compensating for it means
/// reconstructing what *probably* happened from local variables that are no
/// longer in scope. With `Outcome<S>`, the state that the run actually reached
/// is right there in the failure, so compensation is written against data you
/// have.
///
/// ```
/// use berylx::{Error, Outcome};
///
/// #[derive(Clone, Debug, PartialEq)]
/// struct Checkout { charged: bool, refunded: bool }
///
/// let failed = Outcome::Err(
///     Checkout { charged: true, refunded: false },
///     Error::new("shipping_unavailable", "no courier"),
/// );
///
/// // The state is reachable from the failure, so we can see a refund is owed.
/// assert!(failed.state().charged);
/// ```
#[derive(Debug)]
pub enum Outcome<S> {
    /// The node succeeded, producing this state.
    Ok(S),
    /// The node failed, having reached this state.
    Err(S, Error),
}

impl<S> Outcome<S> {
    /// Returns `true` if the node succeeded.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Outcome::Ok(_))
    }

    /// Returns `true` if the node failed.
    #[must_use]
    pub fn is_err(&self) -> bool {
        matches!(self, Outcome::Err(_, _))
    }

    /// Borrows the state the run reached, whether it succeeded or failed.
    ///
    /// This method is the whole point of the type: it is total. There is no
    /// outcome of a berylx workflow from which the state is unavailable.
    #[must_use]
    pub fn state(&self) -> &S {
        match self {
            Outcome::Ok(state) | Outcome::Err(state, _) => state,
        }
    }

    /// Takes the state the run reached, whether it succeeded or failed.
    #[must_use]
    pub fn into_state(self) -> S {
        match self {
            Outcome::Ok(state) | Outcome::Err(state, _) => state,
        }
    }

    /// Borrows the error, if the node failed.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        match self {
            Outcome::Ok(_) => None,
            Outcome::Err(_, error) => Some(error),
        }
    }

    /// Takes the state, discarding the error if there was one.
    #[must_use]
    pub fn ok(self) -> Option<S> {
        match self {
            Outcome::Ok(state) => Some(state),
            Outcome::Err(_, _) => None,
        }
    }

    /// Runs `f` on the state only if the node succeeded.
    #[must_use]
    pub fn and_then(self, f: impl FnOnce(S) -> Outcome<S>) -> Outcome<S> {
        match self {
            Outcome::Ok(state) => f(state),
            failed @ Outcome::Err(_, _) => failed,
        }
    }

    /// Adds `node` to the front of the error's route, if the node failed.
    #[must_use]
    pub fn prepend_trace(self, node: impl Into<String>) -> Outcome<S> {
        match self {
            ok @ Outcome::Ok(_) => ok,
            Outcome::Err(state, error) => Outcome::Err(state, error.prepend_trace(node)),
        }
    }

    /// Splits into a standard [`Result`], keeping the state on both sides.
    ///
    /// # Errors
    ///
    /// Returns the reached state paired with the error when the node failed.
    pub fn into_result(self) -> Result<S, (S, Error)> {
        match self {
            Outcome::Ok(state) => Ok(state),
            Outcome::Err(state, error) => Err((state, error)),
        }
    }
}

impl<S> From<Outcome<S>> for Result<S, (S, Error)> {
    fn from(outcome: Outcome<S>) -> Self {
        outcome.into_result()
    }
}
