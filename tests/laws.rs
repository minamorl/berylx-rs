//! The laws the design rests on, checked rather than claimed.
//!
//! Berylx composes tasks, so composition has to behave. These tests pin the
//! properties that make `.then()` a real composition operator, and — just as
//! importantly — pin the one property that deliberately does *not* hold, so
//! that its absence is a decision rather than a surprise.

use berylx::{run, task, Conflict, Error, Merge, Node, Outcome};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Trail(Vec<&'static str>);

impl Merge for Trail {
    fn merge(_base: &Self, mut left: Self, right: Self) -> Result<Self, Conflict> {
        left.0.extend(right.0);
        Ok(left)
    }
}

fn step(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.0.push(name);
        Outcome::Ok(t)
    })
}

fn boom(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.0.push(name);
        Outcome::Err(t, Error::new("boom", "exploded"))
    })
}

#[test]
fn sequence_composition_is_associative_structurally() {
    // Flattening is what makes the two groupings the same node, not merely
    // nodes that happen to behave alike.
    let left = step("a").then(step("b")).then(step("c"));
    let right = step("a").then(step("b").then(step("c")));

    let names = |node: &Node<Trail>| -> Vec<String> {
        node.tasks().iter().map(|t| t.name().to_owned()).collect()
    };
    assert_eq!(names(&left), names(&right));
    assert!(matches!(left, Node::Seq(ref steps) if steps.len() == 3));
    assert!(matches!(right, Node::Seq(ref steps) if steps.len() == 3));
}

#[test]
fn sequence_composition_is_associative_on_the_success_path() {
    let left = run(&step("a").then(step("b")).then(step("c")), Trail::default());
    let right = run(&step("a").then(step("b").then(step("c"))), Trail::default());

    assert_eq!(left.state(), right.state());
    assert_eq!(left.state().0, vec!["a", "b", "c"]);
}

#[test]
fn sequence_composition_is_associative_on_the_error_path() {
    // Short-circuiting must not depend on how the steps were grouped, and the
    // partial state has to match too — otherwise compensation would differ.
    let left = run(&step("a").then(boom("x")).then(step("c")), Trail::default());
    let right = run(&step("a").then(boom("x").then(step("c"))), Trail::default());

    assert!(left.is_err() && right.is_err());
    assert_eq!(left.state(), right.state());
    assert_eq!(left.state().0, vec!["a", "x"]);
}

#[test]
fn identity_task_is_a_unit_for_sequence() {
    let identity: Node<Trail> = task("identity", Outcome::Ok);

    let bare = run(&step("a"), Trail::default());
    let left_unit = run(&identity.clone().then(step("a")), Trail::default());
    let right_unit = run(&step("a").then(identity), Trail::default());

    assert_eq!(bare.state(), left_unit.state());
    assert_eq!(bare.state(), right_unit.state());
}

#[test]
fn parallel_is_not_commutative_and_that_is_pinned() {
    // Berylx folds branches in written order, so swapping them swaps the
    // result. Being quietly non-commutative would be the worst outcome; being
    // knowably non-commutative lets a caller reason about it.
    let left = run(&step("a").par(step("b")), Trail::default());
    let right = run(&step("b").par(step("a")), Trail::default());

    assert_eq!(left.state().0, vec!["a", "b"]);
    assert_eq!(right.state().0, vec!["b", "a"]);
    assert_ne!(left.state(), right.state());
}

#[test]
fn failure_carries_the_state_it_reached() {
    // The central claim of the crate.
    let outcome = run(&step("a").then(step("b")).then(boom("x")), Trail::default());

    assert!(outcome.is_err());
    assert_eq!(outcome.state().0, vec!["a", "b", "x"]);
}

#[test]
fn error_records_the_failing_node_and_the_route() {
    let workflow = step("a").then(boom("x"));
    let outcome = run(&workflow, Trail::default());

    let error = outcome.error().expect("should have failed");
    assert_eq!(error.code(), "boom");
    assert_eq!(error.failed_node(), Some("x"));
    assert_eq!(error.trace(), ["x"]);
}

#[test]
fn rescue_receives_the_state_the_failure_reached() {
    let workflow = step("a")
        .then(boom("charge"))
        .rescue_with("refund", |mut t: Trail, error| {
            // Compensation is written against real data: the failed charge is
            // visible right here.
            assert_eq!(error.code(), "boom");
            assert!(t.0.contains(&"charge"));
            t.0.push("refund");
            Outcome::Ok(t)
        })
        .then(step("notify"));

    let outcome = run(&workflow, Trail::default());
    assert!(outcome.is_ok());
    assert_eq!(outcome.state().0, vec!["a", "charge", "refund", "notify"]);
}

#[test]
fn a_successful_workflow_never_enters_the_rescue_handler() {
    let workflow = step("a").rescue_with("never", |mut t: Trail, _| {
        t.0.push("never");
        Outcome::Ok(t)
    });

    assert_eq!(run(&workflow, Trail::default()).state().0, vec!["a"]);
}

#[test]
fn rescue_may_decline_to_recover() {
    // `Outcome::Err` is itself the handler: declining to recover is just
    // handing the failure back.
    let workflow = boom("x").rescue_with("give_up", Outcome::Err);
    let outcome = run(&workflow, Trail::default());

    assert!(outcome.is_err());
    assert_eq!(outcome.state().0, vec!["x"]);
}
