//! Parallel branches: really concurrent, deterministically folded.

mod common;

use std::time::{Duration, Instant};

use berylx::{run, task, Error, Node, OnErr, Outcome};
use common::Trail;

fn slow(name: &'static str, millis: u64) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        std::thread::sleep(Duration::from_millis(millis));
        t.push(name);
        Outcome::Ok(t)
    })
}

fn quick(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.push(name);
        Outcome::Ok(t)
    })
}

fn failing(name: &'static str, millis: u64) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        std::thread::sleep(Duration::from_millis(millis));
        t.push(name);
        Outcome::Err(t, Error::new(name, "branch failed"))
    })
}

#[test]
fn branches_run_at_the_same_time() {
    // Three 120ms branches finish in about 120ms, not 360ms. Without real
    // threads this test fails, so it is the actual claim being pinned.
    let workflow = slow("a", 120).par(slow("b", 120)).par(slow("c", 120));

    let started = Instant::now();
    let outcome = run(&workflow, Trail::default());
    let elapsed = started.elapsed();

    assert!(outcome.is_ok());
    assert!(
        elapsed < Duration::from_millis(300),
        "three concurrent 120ms branches took {elapsed:?}; they appear to be sequential"
    );
}

#[test]
fn fold_order_is_definition_order_not_completion_order() {
    // The slow branch is written first, so it folds first — even though the
    // quick one finishes long before it. Wall-clock order must not leak into
    // the state, or the same workflow would produce different results on a
    // loaded machine.
    let workflow = slow("slow", 100).par(quick("quick"));
    let outcome = run(&workflow, Trail::default());

    assert_eq!(outcome.state().as_slice(), ["slow", "quick"]);
}

#[test]
fn parallel_flattens_instead_of_nesting() {
    let workflow = quick("a").par(quick("b")).par(quick("c"));

    match &workflow {
        Node::Par { branches, on_err, .. } => {
            assert_eq!(branches.len(), 3);
            assert_eq!(*on_err, OnErr::ShortCircuit);
        }
        other => panic!("expected a flat parallel node, got {other:?}"),
    }
}

#[test]
fn short_circuit_is_the_default_and_reports_the_first_failure_in_written_order() {
    // Both branches fail; the later-written one fails *sooner* in wall-clock
    // terms. Written order still decides which failure is reported.
    let workflow = failing("first", 100).par(failing("second", 0));
    let outcome = run(&workflow, Trail::default());

    let error = outcome.error().expect("should have failed");
    assert_eq!(error.code(), "first");
    assert!(error.parallel_errors().is_empty());
}

#[test]
fn short_circuit_keeps_the_state_of_the_branch_that_failed() {
    let workflow = quick("ok").par(failing("bad", 0));
    let outcome = run(&workflow, Trail::default());

    assert!(outcome.is_err());
    assert_eq!(outcome.state().as_slice(), ["bad"]);
}

#[test]
fn accumulate_runs_every_branch_and_collects_the_failures() {
    let workflow = failing("one", 0).par(quick("fine")).par(failing("two", 0)).accumulate();
    let outcome = run(&workflow, Trail::default());

    let error = outcome.error().expect("should have failed");
    assert_eq!(error.code(), "parallel_failed");

    let codes: Vec<&str> = error.parallel_errors().iter().map(berylx::Error::code).collect();
    assert_eq!(codes, ["one", "two"]);
}

#[test]
fn accumulate_keeps_the_work_of_every_branch_including_the_failed_ones() {
    // Nothing that happened is discarded — that is what accumulating is for.
    let workflow = failing("one", 0).par(quick("fine")).par(failing("two", 0)).accumulate();
    let outcome = run(&workflow, Trail::default());

    assert_eq!(outcome.state().as_slice(), ["one", "fine", "two"]);
}

#[test]
fn accumulate_succeeds_when_no_branch_fails() {
    let workflow = quick("a").par(quick("b")).accumulate();
    let outcome = run(&workflow, Trail::default());

    assert!(outcome.is_ok());
    assert_eq!(outcome.state().as_slice(), ["a", "b"]);
}

#[test]
fn mode_is_carried_by_the_node_not_by_the_interpreter() {
    // Switching modes is a property of the composed workflow, so the same
    // interpreter produces both behaviours.
    let base = failing("x", 0).par(quick("y"));

    assert_eq!(
        run(&base.clone(), Trail::default()).error().map(|e| e.code().to_owned()),
        Some("x".to_owned())
    );
    assert_eq!(
        run(&base.accumulate(), Trail::default()).error().map(|e| e.code().to_owned()),
        Some("parallel_failed".to_owned())
    );
}
