//! The structured error that travels beside a partial state.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt;

/// A machine-readable failure, carrying where it happened and how it got there.
///
/// A berylx `Error` is deliberately richer than a plain message. Because a
/// failed workflow hands back [`Outcome::Err`](crate::Outcome::Err) instead of
/// unwinding, the error is the only place left to record *which* node failed
/// and *what route* the run took to reach it.
///
/// ```
/// use berylx::Error;
///
/// let err = Error::new("card_declined", "the card was declined")
///     .with_metadata("processor", "stripe");
///
/// assert_eq!(err.code(), "card_declined");
/// assert_eq!(err.metadata().get("processor").map(String::as_str), Some("stripe"));
/// ```
pub struct Error {
    // Boxed so that `Outcome::Err(S, Error)` stays cheap to move. An error
    // this descriptive is wide, and every successful step would otherwise pay
    // for the failure case it did not take.
    inner: Box<Inner>,
}

#[derive(Debug)]
struct Inner {
    code: String,
    message: String,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    failed_node: Option<String>,
    trace: Vec<String>,
    parallel_errors: Vec<Error>,
    metadata: BTreeMap<String, String>,
}

impl Error {
    /// Builds an error from a machine-readable code and a human-readable message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            inner: Box::new(Inner {
                code: code.into(),
                message: message.into(),
                source: None,
                failed_node: None,
                trace: Vec::new(),
                parallel_errors: Vec::new(),
                metadata: BTreeMap::new(),
            }),
        }
    }

    /// Attaches the underlying cause, reported through [`std::error::Error::source`].
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.inner.source = Some(Box::new(source));
        self
    }

    /// Records one free-form key/value pair alongside the failure.
    #[must_use]
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.inner.metadata.insert(key.into(), value.into());
        self
    }

    /// Collects the failures of sibling branches that ran under
    /// [`OnErr::Accumulate`](crate::OnErr::Accumulate).
    #[must_use]
    pub fn with_parallel_errors(mut self, errors: Vec<Error>) -> Self {
        self.inner.parallel_errors = errors;
        self
    }

    /// Pushes `node` onto the front of the route, claiming it as the failing
    /// node if none was claimed yet.
    ///
    /// Each composite re-raises the errors of the nodes it contains, so a
    /// failure deep inside a workflow arrives at the caller with the full path
    /// from the outermost node down to the one that actually failed.
    #[must_use]
    pub fn prepend_trace(mut self, node: impl Into<String>) -> Self {
        let node = node.into();
        if self.inner.failed_node.is_none() {
            self.inner.failed_node = Some(node.clone());
        }
        self.inner.trace.insert(0, node);
        self
    }

    /// The machine-readable code. Match on this, not on the message.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.inner.code
    }

    /// The human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.inner.message
    }

    /// The name of the node that failed, if the error passed through one.
    #[must_use]
    pub fn failed_node(&self) -> Option<&str> {
        self.inner.failed_node.as_deref()
    }

    /// The route from the outermost node down to the failing one.
    #[must_use]
    pub fn trace(&self) -> &[String] {
        &self.inner.trace
    }

    /// The sibling failures gathered by an accumulating parallel node.
    #[must_use]
    pub fn parallel_errors(&self) -> &[Error] {
        &self.inner.parallel_errors
    }

    /// The free-form key/value pairs attached to this failure.
    #[must_use]
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.inner.metadata
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.inner.code, self.inner.message)?;
        if let Some(node) = &self.inner.failed_node {
            write!(f, " (at {node})")?;
        }
        Ok(())
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.inner.source.as_ref().map(|boxed| &**boxed as &(dyn StdError + 'static))
    }
}
