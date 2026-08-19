//! Retry, dry-run and audit are added by wrapping the interpreter — never by
//! editing the workflow. These tests run the *same* workflow value under
//! different interpreters to pin that.

mod common;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use berylx::{
    branch, run, run_with, task, Audit, Backoff, DryRun, Error, Node, Outcome, Real, Retry, Root,
};
use common::Trail;

fn step(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.push(name);
        Outcome::Ok(t)
    })
}

/// The one workflow value every test in this file reuses.
fn workflow() -> Node<Trail> {
    step("charge").then(step("ship")).then(step("notify"))
}

#[test]
fn dry_run_lists_the_plan_without_performing_anything() {
    let plan = DryRun::new();
    let outcome = run_with(&workflow(), Trail::default(), &plan);

    assert_eq!(plan.planned(), ["charge", "ship", "notify"]);
    // No task body ran, so the state is untouched.
    assert_eq!(outcome.state().0, Vec::<String>::new());
}

#[test]
fn the_real_interpreter_runs_the_same_workflow_for_real() {
    let outcome = run_with(&workflow(), Trail::default(), &Real);
    assert_eq!(outcome.state().as_slice(), ["charge", "ship", "notify"]);
}

#[test]
fn audit_sees_every_task_and_still_performs_the_work() {
    let audit = Audit::new(Real);
    let outcome = run_with(&workflow(), Trail::default(), &audit);

    assert_eq!(audit.seen(), ["charge", "ship", "notify"]);
    assert_eq!(outcome.state().as_slice(), ["charge", "ship", "notify"]);
}

#[test]
fn dry_run_still_evaluates_predicates_because_the_route_is_part_of_the_plan() {
    let workflow =
        branch().when("empty", |t: &Trail| t.0.is_empty(), step("greet")).otherwise(step("resume"));

    let plan = DryRun::new();
    run_with(&workflow, Trail::default(), &plan);

    assert_eq!(plan.planned(), ["greet"]);
}

#[test]
fn dry_run_of_a_parallel_workflow_lists_every_branch() {
    let plan = DryRun::new();
    run_with(&step("a").par(step("b")).par(step("c")), Trail::default(), &plan);

    let mut planned = plan.planned();
    planned.sort();
    assert_eq!(planned, ["a", "b", "c"]);
}

#[test]
fn retry_gives_a_flaky_task_another_go() {
    let attempts = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&attempts);

    let flaky: Node<Trail> = task("flaky", move |mut t: Trail| {
        let n = counter.fetch_add(1, Ordering::SeqCst);
        t.push("attempt");
        if n < 2 {
            Outcome::Err(t, Error::new("flaky", "not yet"))
        } else {
            Outcome::Ok(t)
        }
    });

    let outcome = run_with(&flaky, Trail::default(), &Retry::without_backoff(Real, 3));

    assert!(outcome.is_ok());
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn retry_hands_each_attempt_the_state_the_last_one_reached() {
    let flaky: Node<Trail> = task("flaky", move |mut t: Trail| {
        t.push("try");
        if t.0.len() < 3 {
            Outcome::Err(t, Error::new("flaky", "not yet"))
        } else {
            Outcome::Ok(t)
        }
    });

    let outcome = run_with(&flaky, Trail::default(), &Retry::without_backoff(Real, 5));

    assert!(outcome.is_ok());
    assert_eq!(outcome.state().as_slice(), ["try", "try", "try"]);
}

#[test]
fn retry_gives_up_and_returns_the_last_failure() {
    let always: Node<Trail> = task("always", |t: Trail| Outcome::Err(t, Error::new("nope", "no")));
    let outcome = run_with(&always, Trail::default(), &Retry::without_backoff(Real, 2));

    assert_eq!(outcome.error().map(berylx::Error::code), Some("nope"));
}

#[test]
fn interpreters_compose_by_wrapping() {
    // Audit around Retry around Real: the audit log shows every attempt.
    let flaky_count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&flaky_count);
    let flaky: Node<Trail> = task("flaky", move |t: Trail| {
        if counter.fetch_add(1, Ordering::SeqCst) < 1 {
            Outcome::Err(t, Error::new("flaky", "not yet"))
        } else {
            Outcome::Ok(t)
        }
    });

    let audit = Audit::new(Retry::without_backoff(Real, 3));
    let outcome = run_with(&flaky, Trail::default(), &audit);

    assert!(outcome.is_ok());
    assert_eq!(audit.seen(), ["flaky"]); // one task, retried inside
    assert_eq!(flaky_count.load(Ordering::SeqCst), 2);
}

#[test]
fn root_commits_on_success() {
    let mut root = Root::new(Trail::default());
    let outcome = root.run(&workflow());

    assert!(outcome.is_ok());
    assert_eq!(root.state().as_slice(), ["charge", "ship", "notify"]);
}

#[test]
fn root_does_not_commit_a_failed_run_but_the_partial_state_is_still_returned() {
    let failing = step("charge")
        .then(task("ship", |t: Trail| Outcome::Err(t, Error::new("no_courier", "none"))));

    let mut root = Root::new(Trail::default());
    let outcome = root.run(&failing);

    // The run got as far as `charge` ...
    assert_eq!(outcome.state().as_slice(), ["charge"]);
    // ... but that never became committed truth.
    assert_eq!(root.state().0, Vec::<String>::new());
}

#[test]
fn a_recovered_run_does_commit() {
    let recovered = step("charge")
        .then(task("ship", |t: Trail| Outcome::Err(t, Error::new("no_courier", "none"))))
        .rescue_with("refund", |mut t: Trail, _| {
            t.push("refund");
            Outcome::Ok(t)
        });

    let mut root = Root::new(Trail::default());
    assert!(root.run(&recovered).is_ok());
    assert_eq!(root.state().as_slice(), ["charge", "refund"]);
}

#[test]
fn a_dry_run_through_the_root_commits_nothing_meaningful() {
    let mut root = Root::new(Trail::default());
    let plan = DryRun::new();
    root.run_with(&workflow(), &plan);

    assert_eq!(plan.planned(), ["charge", "ship", "notify"]);
    assert_eq!(root.state().0, Vec::<String>::new());
}

#[test]
fn branch_takes_the_first_matching_arm() {
    let workflow = branch()
        .when("never", |_: &Trail| false, step("skipped"))
        .when("always", |_: &Trail| true, step("taken"))
        .when("also_true", |_: &Trail| true, step("not_reached"))
        .end();

    assert_eq!(run(&workflow, Trail::default()).state().as_slice(), ["taken"]);
}

#[test]
fn branch_without_a_match_and_without_a_fallback_is_a_no_op() {
    let workflow = branch().when("never", |_: &Trail| false, step("skipped")).end();
    let outcome = run(&workflow, Trail::default());

    assert!(outcome.is_ok());
    assert_eq!(outcome.state().0, Vec::<String>::new());
}

#[test]
fn branch_records_the_arm_it_took_in_the_error_route() {
    let workflow = branch()
        .when(
            "risky",
            |_: &Trail| true,
            task("explode", |t: Trail| Outcome::Err(t, Error::new("boom", "no"))),
        )
        .end();

    let outcome = run(&workflow, Trail::default());
    let error = outcome.error().expect("should have failed");

    // The route reads outermost-first: the arm that was taken, then the task
    // inside it that actually failed.
    assert_eq!(error.failed_node(), Some("explode"));
    assert_eq!(error.trace(), ["risky", "explode"]);
}

#[test]
fn retry_waits_between_attempts_by_default() {
    // The default is exponential backoff with jitter, so a retry loop cannot
    // hammer a struggling dependency. Three attempts means two waits, of at
    // least base and 2*base, each scaled by a jitter factor of at least 0.5.
    let always: Node<Trail> = task("always", |t: Trail| Outcome::Err(t, Error::new("nope", "no")));

    let retry = Retry::new(Real, 3).with_base_delay(Duration::from_millis(40));
    let started = Instant::now();
    let outcome = run_with(&always, Trail::default(), &retry);
    let elapsed = started.elapsed();

    assert!(outcome.is_err());
    // Minimum possible wait: 0.5*40 + 0.5*80 = 60ms.
    assert!(
        elapsed >= Duration::from_millis(60),
        "expected the default Retry to back off, but it took only {elapsed:?}"
    );
}

#[test]
fn without_backoff_does_not_wait() {
    let always: Node<Trail> = task("always", |t: Trail| Outcome::Err(t, Error::new("nope", "no")));

    let started = Instant::now();
    run_with(&always, Trail::default(), &Retry::without_backoff(Real, 5));
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_millis(20), "immediate retry took {elapsed:?}");
}

#[test]
fn the_default_backoff_is_exponential_jitter() {
    assert_eq!(Backoff::default(), Backoff::ExponentialJitter);
}
