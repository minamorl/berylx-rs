//! Behaviour berylx-rs follows from the public contract of berylx-ts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use berylx::{
    branch, keep_left, keep_right, run, run_with, task, Conflict, DryRun, Error, Merge, Node,
    Outcome, Root,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Account {
    status: Option<&'static str>,
    visits: u32,
}

impl Merge for Account {
    fn merge(base: &Self, left: Self, right: Self) -> Result<Self, Conflict> {
        let status = if left.status == base.status {
            right.status
        } else if right.status == base.status || right.status == left.status {
            left.status
        } else {
            return Err(Conflict::new("status was set to two different values"));
        };
        Ok(Account { status, visits: left.visits + right.visits - base.visits })
    }
}

fn set_status(name: &'static str, status: &'static str) -> Node<Account> {
    task(name, move |account: Account| Outcome::Ok(Account { status: Some(status), ..account }))
}

fn visit() -> Node<Account> {
    task("visit", |account: Account| Outcome::Ok(Account { visits: account.visits + 1, ..account }))
}

const START: Account = Account { status: None, visits: 3 };

fn checkout() -> Node<i32> {
    task("charge", |n: i32| Outcome::Ok(n + 1)).then(
        branch()
            .when("paid", |n: &i32| *n > 0, task("ship", |n: i32| Outcome::Ok(n * 10)))
            .otherwise(task("refund", |n: i32| Outcome::Ok(n - 100))),
    )
}

#[test]
fn the_unconditional_final_arm_runs_only_when_no_arm_matched() {
    let workflow = branch()
        .when("big", |n: &i32| *n > 10, task("halve", |n: i32| Outcome::Ok(n / 2)))
        .when("odd", |n: &i32| n % 2 == 1, task("bump", |n: i32| Outcome::Ok(n + 1)))
        .otherwise(task("double", |n: i32| Outcome::Ok(n * 2)));

    assert_eq!(run(&workflow, 20).ok(), Some(10));
    assert_eq!(run(&workflow, 3).ok(), Some(4));
    assert_eq!(run(&workflow, 4).ok(), Some(8));
}

#[test]
fn the_root_commits_the_whole_run_and_exposes_it() {
    let mut root = Root::new(0);

    assert_eq!(root.run(&checkout()).ok(), Some(10));
    assert_eq!(*root.state(), 10);
}

#[test]
fn running_without_a_root_commits_nowhere() {
    let root = Root::new(5);

    assert_eq!(run(&checkout(), *root.state()).ok(), Some(60));
    assert_eq!(*root.state(), 5);
}

#[test]
fn a_dry_run_plan_belongs_to_that_run_alone() {
    let workflow = checkout();

    assert_eq!(DryRun::plan(&workflow, 1), vec!["charge", "ship"]);
    assert_eq!(DryRun::plan(&workflow, 1), vec!["charge", "ship"]);
}

#[test]
fn a_dry_run_evaluates_predicates_against_the_supplied_state() {
    // A real run of `charge` would turn 0 into 1 and take the "paid" arm; the
    // dry run does not simulate that update.
    assert_eq!(run(&checkout(), 0).ok(), Some(10));
    assert_eq!(DryRun::plan(&checkout(), 0), vec!["charge", "refund"]);
}

#[test]
fn a_dry_run_never_invokes_a_recovery_handler() {
    let recovered = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&recovered);
    let workflow = checkout().rescue_with("undo", move |state: i32, _: Error| {
        seen.store(true, Ordering::SeqCst);
        Outcome::Ok(state)
    });

    let dry = DryRun::new();
    assert_eq!(run_with(&workflow, 0, &dry).ok(), Some(0));
    assert_eq!(DryRun::plan(&workflow, 0), vec!["charge", "refund"]);
    assert!(!recovered.load(Ordering::SeqCst));
}

#[test]
fn the_one_call_dry_run_returns_only_that_call_plan() {
    let first = task("a", |n: i32| Outcome::Ok(n)).then(task("b", |n: i32| Outcome::Ok(n)));
    let second = task("c", |n: i32| Outcome::Ok(n));

    assert_eq!(DryRun::plan(&first, 0), vec!["a", "b"]);
    assert_eq!(DryRun::plan(&second, 0), vec!["c"]);
}

#[test]
fn every_join_sees_the_state_all_branches_started_from() {
    let bases = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&bases);
    let join = move |base: &i32, left: i32, right: i32| {
        seen.lock().unwrap().push(*base);
        Ok(left + right - base)
    };
    let workflow = task("a", |n: i32| Outcome::Ok(n + 1))
        .par_with(task("b", |n: i32| Outcome::Ok(n + 10)), join.clone())
        .par_with(task("c", |n: i32| Outcome::Ok(n + 100)), join);

    assert_eq!(run(&workflow, 5).ok(), Some(116));
    // A binary left fold over three branches joins twice, each time from 5.
    assert_eq!(*bases.lock().unwrap(), vec![5, 5]);
}

#[test]
fn independent_updates_from_parallel_branches_are_both_kept() {
    let workflow = set_status("pay", "paid").par(visit());

    assert_eq!(run(&workflow, START).ok(), Some(Account { status: Some("paid"), visits: 4 }));
}

#[test]
fn a_merge_conflict_fails_with_its_code_and_the_base_state() {
    let workflow = set_status("pay", "paid").par(set_status("trial", "trial"));

    match run(&workflow, START) {
        Outcome::Err(state, error) => {
            assert_eq!(state, START);
            assert_eq!(error.code(), "merge_conflict");
        }
        Outcome::Ok(state) => panic!("expected a merge conflict, got {state:?}"),
    }
}

#[test]
fn a_merge_conflict_is_not_committed_by_the_root() {
    let mut root = Root::new(START);
    let workflow = set_status("pay", "paid").par(set_status("trial", "trial"));

    assert!(root.run(&workflow).ok().is_none());
    assert_eq!(*root.state(), START);
}

#[test]
fn keep_left_and_keep_right_are_chosen_explicitly() {
    let branches = |join: fn(&i32, i32, i32) -> Result<i32, Conflict>| {
        task("a", |n: i32| Outcome::Ok(n + 1))
            .par_with(task("b", |n: i32| Outcome::Ok(n + 10)), join)
            .par_with(task("c", |n: i32| Outcome::Ok(n + 100)), join)
    };

    assert_eq!(run(&branches(keep_left), 0).ok(), Some(1));
    assert_eq!(run(&branches(keep_right), 0).ok(), Some(100));
}
