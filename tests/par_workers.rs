//! Seam tests wiring `Node::Par` and the synchronous interpreter to `workers::run`.
//!
//! Each test cites the `berylx-rs.spec` v0.5 pin(s) it checks.

mod common;

use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier, Mutex};
use std::thread;

use berylx::{
    run, run_with, task, to_dot, to_mermaid, Audit, DryRun, Error, Merge, Node, OnErr, Outcome,
    Real, Retry, Root,
};
use common::Trail;

fn nz(n: usize) -> NonZeroUsize {
    NonZeroUsize::new(n).expect("limit must be positive")
}

fn step(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.push(name);
        Outcome::Ok(t)
    })
}

fn fail(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.push(name);
        Outcome::Err(t, Error::new(name, "branch failed"))
    })
}

struct SignalOnDrop(Option<mpsc::Sender<()>>);

impl Drop for SignalOnDrop {
    fn drop(&mut self) {
        if let Some(tx) = self.0.take() {
            let _ = tx.send(());
        }
    }
}

thread_local! {
    /// Fires strictly when the current worker OS thread exits — i.e., after
    /// `workers::run` has returned from `catch_unwind`, set the lock-free stop
    /// flag, recorded the panic under `queue.lock()`, released the lock, and
    /// broken out of the worker loop.
    static ON_THREAD_EXIT: RefCell<Option<SignalOnDrop>> = const { RefCell::new(None) };
}

fn signal_on_thread_exit(tx: mpsc::Sender<()>) {
    ON_THREAD_EXIT.with(|slot| {
        *slot.borrow_mut() = Some(SignalOnDrop(Some(tx)));
    });
}

/// Pin: `parallel.workers.option`
#[test]
fn parallel_workers_option_defaults_none_and_attaches_to_par() {
    let unlimited = step("a").par(step("b"));
    assert!(matches!(unlimited, Node::Par { workers: None, .. }));

    let bounded = unlimited.with_workers(nz(1));
    assert!(matches!(bounded, Node::Par { workers: Some(w), .. } if w.get() == 1));

    // Preserved across `.accumulate()` and `.short_circuit()`.
    let acc = bounded.clone().accumulate();
    assert!(
        matches!(acc, Node::Par { on_err: OnErr::Accumulate, workers: Some(w), .. } if w.get() == 1)
    );
    let sc = acc.short_circuit();
    assert!(
        matches!(sc, Node::Par { on_err: OnErr::ShortCircuit, workers: Some(w), .. } if w.get() == 1)
    );

    // Works with `par_with` and is a no-op on non-Par nodes.
    let via_par_with = step("a").par_with(step("b"), Trail::merge).with_workers(nz(2));
    assert!(matches!(via_par_with, Node::Par { workers: Some(w), .. } if w.get() == 2));

    let single = step("only").with_workers(nz(1));
    assert!(matches!(single, Node::Task(_)));
    let seq = step("a").then(step("b")).with_workers(nz(1));
    assert!(matches!(seq, Node::Seq(_)));

    // Empty Par node with a worker limit still succeeds without modifying state.
    let empty: Node<Trail> = Node::Par {
        branches: Vec::new(),
        merge: Arc::new(Trail::merge),
        on_err: OnErr::ShortCircuit,
        workers: Some(nz(2)),
    };
    let mut start = Trail::default();
    start.push("keep");
    let out = run(&empty, start);
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["keep"]);
}

/// Pin: `parallel.workers.composed`
#[test]
fn parallel_workers_composed_attaches_to_flattened_par_node() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let first_wave = Arc::new(Barrier::new(2));

    let make = |idx: usize, name: &'static str| {
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let first_wave = Arc::clone(&first_wave);
        task(name, move |mut t: Trail| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now, Ordering::SeqCst);
            if idx < 2 {
                first_wave.wait();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    };

    let workflow =
        make(0, "a").par(make(1, "b")).par(make(2, "c")).par(make(3, "d")).with_workers(nz(2));

    match &workflow {
        Node::Par { branches, workers, .. } => {
            assert_eq!(branches.len(), 4);
            assert_eq!(*workers, Some(nz(2)));
        }
        other => panic!("expected flattened Par, got {other:?}"),
    }

    let out = run(&workflow, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["a", "b", "c", "d"]);
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
}

/// Pin: `compose.parallel_flattens`
#[test]
fn compose_parallel_flattens_unlimited_and_preserves_bounded_as_branch() {
    // Unlimited par still flattens on both left and right.
    let flat_left = step("a").par(step("b")).par(step("c"));
    assert!(matches!(&flat_left, Node::Par { branches, workers: None, .. } if branches.len() == 3));

    let flat_right = step("a").par(step("b").par(step("c")));
    assert!(
        matches!(&flat_right, Node::Par { branches, workers: None, .. } if branches.len() == 3)
    );

    // Bounded par on the left is preserved as a single child branch.
    let bounded_left = step("a").par(step("b")).with_workers(nz(1)).par(step("c"));
    match &bounded_left {
        Node::Par { branches, workers: None, .. } => {
            assert_eq!(branches.len(), 2);
            assert!(
                matches!(&branches[0], Node::Par { branches: inner, workers: Some(w), .. } if inner.len() == 2 && w.get() == 1)
            );
            assert!(matches!(&branches[1], Node::Task(_)));
        }
        other => panic!("expected outer unlimited Par, got {other:?}"),
    }

    // Bounded par on the right is preserved as a single child branch.
    let bounded_right = step("a").par(step("b").par(step("c")).with_workers(nz(2)));
    match &bounded_right {
        Node::Par { branches, workers: None, .. } => {
            assert_eq!(branches.len(), 2);
            assert!(matches!(&branches[0], Node::Task(_)));
            assert!(
                matches!(&branches[1], Node::Par { branches: inner, workers: Some(w), .. } if inner.len() == 2 && w.get() == 2)
            );
        }
        other => panic!("expected outer unlimited Par, got {other:?}"),
    }

    // Bounded par on both sides preserves both as separate branches.
    let both_bounded = step("a")
        .par(step("b"))
        .with_workers(nz(1))
        .par(step("c").par(step("d")).with_workers(nz(2)));
    match &both_bounded {
        Node::Par { branches, workers: None, .. } => {
            assert_eq!(branches.len(), 2);
            assert!(matches!(&branches[0], Node::Par { workers: Some(w), .. } if w.get() == 1));
            assert!(matches!(&branches[1], Node::Par { workers: Some(w), .. } if w.get() == 2));
        }
        other => panic!("expected outer Par with two bounded children, got {other:?}"),
    }

    // Execution, task enumeration, and rendering all work on nested bounded Par.
    assert_eq!(run(&both_bounded, Trail::default()).state().as_slice(), ["a", "b", "c", "d"]);
    let names: Vec<&str> = both_bounded.tasks().iter().map(|t| t.name()).collect();
    assert_eq!(names, ["a", "b", "c", "d"]);
    assert!(to_dot(&both_bounded).contains("label=\"d\""));
    assert!(to_mermaid(&both_bounded).contains("\"d\""));
}

/// Pins: `parallel.workers.valid`, `parallel.workers.invalid`, `parallel.workers.argument_type`
#[test]
fn parallel_workers_argument_type_is_non_zero_usize() {
    // Compile-time proof that the argument type is strictly `NonZeroUsize`,
    // making zero and negative integers unrepresentable as arguments.
    let attach: fn(Node<Trail>, NonZeroUsize) -> Node<Trail> = Node::with_workers;
    assert!(NonZeroUsize::new(0).is_none());

    // Valid positive limits including 1, < branch count, == branch count, and > branch count.
    for limit in [1, 2, 3, 10] {
        let workflow = attach(step("a").par(step("b")).par(step("c")), nz(limit));
        let out = run(&workflow, Trail::default());
        assert!(out.is_ok());
        assert_eq!(out.state().as_slice(), ["a", "b", "c"]);
    }
}

/// Pin: `parallel.workers.default`
#[test]
fn parallel_workers_default_starts_all_branches_together() {
    let barrier = Arc::new(Barrier::new(4));
    let make = |name: &'static str| {
        let barrier = Arc::clone(&barrier);
        task(name, move |mut t: Trail| {
            // Only completes if all 4 branches are running simultaneously.
            barrier.wait();
            t.push(name);
            Outcome::Ok(t)
        })
    };

    let workflow = make("a").par(make("b")).par(make("c")).par(make("d"));
    let out = run(&workflow, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["a", "b", "c", "d"]);
}

/// Pin: `parallel.workers.bound`
#[test]
fn parallel_workers_bound_caps_concurrently_running_branches() {
    // Limit = 2 across 6 branches: reaches 2 and never exceeds 2.
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let wave = Arc::new(Barrier::new(2));

    let mut iter = ["b0", "b1", "b2", "b3", "b4", "b5"].into_iter().enumerate().map(|(i, name)| {
        let active = Arc::clone(&active);
        let max_active = Arc::clone(&max_active);
        let wave = Arc::clone(&wave);
        task(name, move |mut t: Trail| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now, Ordering::SeqCst);
            if i < 2 {
                wave.wait();
            }
            active.fetch_sub(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    });
    let first = iter.next().unwrap();
    let workflow = iter.fold(first, Node::par).with_workers(nz(2));
    let out = run(&workflow, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["b0", "b1", "b2", "b3", "b4", "b5"]);
    assert_eq!(max_active.load(Ordering::SeqCst), 2);

    // Limit = 1 across 4 branches: never exceeds 1.
    let active_one = Arc::new(AtomicUsize::new(0));
    let max_one = Arc::new(AtomicUsize::new(0));
    let make_one = |name: &'static str| {
        let active_one = Arc::clone(&active_one);
        let max_one = Arc::clone(&max_one);
        task(name, move |mut t: Trail| {
            let now = active_one.fetch_add(1, Ordering::SeqCst) + 1;
            max_one.fetch_max(now, Ordering::SeqCst);
            thread::yield_now();
            active_one.fetch_sub(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    };
    let serial =
        make_one("a").par(make_one("b")).par(make_one("c")).par(make_one("d")).with_workers(nz(1));
    assert_eq!(run(&serial, Trail::default()).state().as_slice(), ["a", "b", "c", "d"]);
    assert_eq!(max_one.load(Ordering::SeqCst), 1);
}

/// Pin: `parallel.workers.scheduling`
#[test]
fn parallel_workers_scheduling_delays_start_only_without_changing_outcome() {
    // Prove branches beyond the worker limit cannot start while the first wave holds all slots.
    let first_wave = Arc::new(Barrier::new(2));
    let release_wave = Arc::new(Barrier::new(2));
    let later_started = Arc::new(AtomicUsize::new(0));

    let make_first = |name: &'static str| {
        let first_wave = Arc::clone(&first_wave);
        let release_wave = Arc::clone(&release_wave);
        let later_started = Arc::clone(&later_started);
        task(name, move |mut t: Trail| {
            first_wave.wait();
            assert_eq!(later_started.load(Ordering::SeqCst), 0);
            release_wave.wait();
            t.push(name);
            Outcome::Ok(t)
        })
    };
    let make_later = |name: &'static str| {
        let later_started = Arc::clone(&later_started);
        task(name, move |mut t: Trail| {
            later_started.fetch_add(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    };

    let delayed = make_first("a")
        .par(make_first("b"))
        .par(make_later("c"))
        .par(make_later("d"))
        .with_workers(nz(2));
    let out = run(&delayed, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["a", "b", "c", "d"]);
    assert_eq!(later_started.load(Ordering::SeqCst), 2);

    // Same outcome across unlimited and bounded worker counts for Ok, ShortCircuit Err, Accumulate Err.
    let ok_base = step("a").par(step("b")).par(step("c")).par(step("d"));
    let sc_base = step("a").par(fail("b")).par(step("c")).par(fail("d"));
    let acc_base = sc_base.clone().accumulate();

    let ok_expected = run(&ok_base, Trail::default()).into_state();
    let sc_expected = run(&sc_base, Trail::default());
    let acc_expected = run(&acc_base, Trail::default());

    for limit in [1, 2, 4] {
        let ok = run(&ok_base.clone().with_workers(nz(limit)), Trail::default());
        assert!(ok.is_ok());
        assert_eq!(ok.into_state(), ok_expected);

        let sc = run(&sc_base.clone().with_workers(nz(limit)), Trail::default());
        assert!(sc.is_err());
        assert_eq!(sc.state(), sc_expected.state());
        assert_eq!(sc.error().map(Error::code), sc_expected.error().map(Error::code));

        let acc = run(&acc_base.clone().with_workers(nz(limit)), Trail::default());
        assert!(acc.is_err());
        assert_eq!(acc.state(), acc_expected.state());
        assert_eq!(acc.error().map(Error::code), acc_expected.error().map(Error::code));
        let got_codes: Vec<&str> =
            acc.error().unwrap().parallel_errors().iter().map(Error::code).collect();
        let exp_codes: Vec<&str> =
            acc_expected.error().unwrap().parallel_errors().iter().map(Error::code).collect();
        assert_eq!(got_codes, exp_codes);
    }
}

/// Pin: `parallel.workers.all_run`
#[test]
fn parallel_workers_all_run_to_completion_even_on_err_and_short_circuit() {
    for (mode, limit) in [
        (OnErr::ShortCircuit, 1),
        (OnErr::ShortCircuit, 2),
        (OnErr::Accumulate, 1),
        (OnErr::Accumulate, 2),
    ] {
        let completed = Arc::new(AtomicUsize::new(0));
        let make = |name: &'static str, should_fail: bool| {
            let completed = Arc::clone(&completed);
            task(name, move |mut t: Trail| {
                t.push(name);
                completed.fetch_add(1, Ordering::SeqCst);
                if should_fail {
                    Outcome::Err(t, Error::new(name, "failed"))
                } else {
                    Outcome::Ok(t)
                }
            })
        };

        let mut workflow = make("b0", true)
            .par(make("b1", false))
            .par(make("b2", true))
            .par(make("b3", false))
            .with_workers(nz(limit));
        if mode == OnErr::Accumulate {
            workflow = workflow.accumulate();
        }

        let out = run(&workflow, Trail::default());
        assert!(out.is_err());
        assert_eq!(
            completed.load(Ordering::SeqCst),
            4,
            "every branch must complete under mode={mode:?} limit={limit}"
        );
    }
}

/// Pin: `parallel.workers.input`
#[test]
fn parallel_workers_input_gives_every_branch_same_start_state() {
    let make = |name: &'static str| {
        task(name, move |mut t: Trail| {
            assert_eq!(t.as_slice(), ["seed"], "branch {name} saw mutated state");
            t.push(name);
            Outcome::Ok(t)
        })
    };

    // With workers = 1 branches execute strictly sequentially, proving delayed
    // branches still receive the original start state rather than chained output.
    let workflow = make("b0").par(make("b1")).par(make("b2")).par(make("b3")).with_workers(nz(1));
    let mut start = Trail::default();
    start.push("seed");

    let out = run(&workflow, start);
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["seed", "b0", "seed", "b1", "seed", "b2", "seed", "b3"]);
}

/// Pin: `parallel.workers.fold`
#[test]
fn parallel_workers_fold_uses_definition_order_not_completion_order() {
    // Force branch 1 to finish before branch 0 while both occupy the 2 worker slots.
    let (tx0, rx0) = mpsc::channel();
    let tx0 = Mutex::new(tx0);
    let rx0 = Mutex::new(rx0);

    let b0 = task("b0", move |mut t: Trail| {
        rx0.lock().unwrap().recv().unwrap();
        t.push("b0");
        Outcome::Ok(t)
    });
    let b1 = task("b1", move |mut t: Trail| {
        t.push("b1");
        tx0.lock().unwrap().send(()).unwrap();
        Outcome::Ok(t)
    });

    let workflow = b0.par(b1).par(step("b2")).par(step("b3")).with_workers(nz(2));
    let out = run(&workflow, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["b0", "b1", "b2", "b3"]);
}

/// Pin: `parallel.workers.short_circuit`
#[test]
fn parallel_workers_short_circuit_reports_first_error_in_definition_order() {
    // Branch 1 fails strictly before branch 0 starts failing; branch 0 still wins.
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let rx = Mutex::new(rx);

    let b0 = task("b0", move |mut t: Trail| {
        rx.lock().unwrap().recv().unwrap();
        t.push("b0");
        Outcome::Err(t, Error::new("err0", "first in definition order"))
    });
    let b1 = task("b1", move |mut t: Trail| {
        t.push("b1");
        tx.lock().unwrap().send(()).unwrap();
        Outcome::Err(t, Error::new("err1", "finished sooner"))
    });

    let workflow = b0.par(b1).par(step("b2")).par(fail("err3")).with_workers(nz(2));
    let out = run(&workflow, Trail::default());
    assert!(out.is_err());
    let err = out.error().expect("expected short-circuit error");
    assert_eq!(err.code(), "err0");
    assert_eq!(err.failed_node(), Some("b0"));
    assert!(err.parallel_errors().is_empty());
    assert_eq!(out.state().as_slice(), ["b0"]);
}

/// Pin: `parallel.workers.accumulate`
#[test]
fn parallel_workers_accumulate_reports_all_errors_in_definition_order() {
    // Force branch 1 to finish before branch 0; both fail, plus branch 3 fails in wave 2.
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let rx = Mutex::new(rx);

    let b0 = task("b0", move |mut t: Trail| {
        rx.lock().unwrap().recv().unwrap();
        t.push("b0");
        Outcome::Err(t, Error::new("err0", "first"))
    });
    let b1 = task("b1", move |mut t: Trail| {
        t.push("b1");
        tx.lock().unwrap().send(()).unwrap();
        Outcome::Err(t, Error::new("err1", "second"))
    });

    let workflow = b0.par(b1).par(step("b2")).par(fail("err3")).accumulate().with_workers(nz(2));
    let out = run(&workflow, Trail::default());
    assert!(out.is_err());

    let err = out.error().expect("expected accumulate error");
    assert_eq!(err.code(), "parallel_failed");
    assert_eq!(err.failed_node(), None);
    let codes: Vec<&str> = err.parallel_errors().iter().map(Error::code).collect();
    assert_eq!(codes, ["err0", "err1", "err3"]);
    assert_eq!(out.state().as_slice(), ["b0", "b1", "b2", "err3"]);
}

/// Pin: `parallel.workers.scope`
#[test]
fn parallel_workers_scope_is_per_parallel_node_with_no_global_pool() {
    // Two independent `run()` calls each have `workers = 1`, yet their active
    // branches can synchronize on a 2-party barrier — impossible with a shared
    // 1-worker global pool.
    let cross_call = Arc::new(Barrier::new(2));
    let make_workflow = |tag: &'static str| {
        let barrier = Arc::clone(&cross_call);
        let first = task(tag, move |mut t: Trail| {
            barrier.wait();
            t.push(tag);
            Outcome::Ok(t)
        });
        first.par(step("tail")).with_workers(nz(1))
    };

    let w1 = make_workflow("call1");
    let w2 = make_workflow("call2");

    let (o1, o2) = thread::scope(|s| {
        let h1 = s.spawn(|| run(&w1, Trail::default()));
        let h2 = s.spawn(|| run(&w2, Trail::default()));
        (h1.join().unwrap(), h2.join().unwrap())
    });

    assert_eq!(o1.state().as_slice(), ["call1", "tail"]);
    assert_eq!(o2.state().as_slice(), ["call2", "tail"]);

    // Two sequential Par nodes in one workflow have independent limits.
    let second_barrier = Arc::new(Barrier::new(3));
    let make_sync = |name: &'static str| {
        let barrier = Arc::clone(&second_barrier);
        task(name, move |mut t: Trail| {
            barrier.wait();
            t.push(name);
            Outcome::Ok(t)
        })
    };
    let seq = step("p1a")
        .par(step("p1b"))
        .with_workers(nz(1))
        .then(make_sync("p2a").par(make_sync("p2b")).par(make_sync("p2c")).with_workers(nz(3)));
    let out = run(&seq, Trail::default());
    assert!(out.is_ok());
    assert_eq!(
        out.state().as_slice(),
        ["p1a", "p1b", "p2a", "p1a", "p1b", "p2b", "p1a", "p1b", "p2c"]
    );
}

/// Pin: `parallel.workers.nested`
#[test]
fn parallel_workers_nested_limits_apply_per_level() {
    // Case 1: outer limit = 1, inner limit = 2 with a 2-party barrier inside the inner Par.
    // Passes only because the inner level owns its own worker budget of 2.
    let inner_barrier = Arc::new(Barrier::new(2));
    let make_inner = |name: &'static str| {
        let barrier = Arc::clone(&inner_barrier);
        task(name, move |mut t: Trail| {
            barrier.wait();
            t.push(name);
            Outcome::Ok(t)
        })
    };
    let inner = make_inner("i0").par(make_inner("i1")).with_workers(nz(2));
    let outer = inner.par(step("o1")).with_workers(nz(1));
    assert_eq!(run(&outer, Trail::default()).state().as_slice(), ["i0", "i1", "o1"]);

    // Case 2: outer limit = 2 runs two inner Par nodes simultaneously (outer barrier = 2),
    // while each inner Par enforces its own limit = 1 on its own branches.
    let outer_barrier = Arc::new(Barrier::new(2));
    let left_active = Arc::new(AtomicUsize::new(0));
    let left_max = Arc::new(AtomicUsize::new(0));
    let right_active = Arc::new(AtomicUsize::new(0));
    let right_max = Arc::new(AtomicUsize::new(0));

    let make_bounded_branch = |name: &'static str,
                               wait_outer: bool,
                               active: &Arc<AtomicUsize>,
                               max: &Arc<AtomicUsize>| {
        let barrier = Arc::clone(&outer_barrier);
        let active = Arc::clone(active);
        let max = Arc::clone(max);
        task(name, move |mut t: Trail| {
            let now = active.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(now, Ordering::SeqCst);
            if wait_outer {
                barrier.wait();
            }
            thread::yield_now();
            active.fetch_sub(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    };

    let left_inner = make_bounded_branch("l0", true, &left_active, &left_max)
        .par(make_bounded_branch("l1", false, &left_active, &left_max))
        .par(make_bounded_branch("l2", false, &left_active, &left_max))
        .with_workers(nz(1));
    let right_inner = make_bounded_branch("r0", true, &right_active, &right_max)
        .par(make_bounded_branch("r1", false, &right_active, &right_max))
        .par(make_bounded_branch("r2", false, &right_active, &right_max))
        .with_workers(nz(1));

    let nested = left_inner.par(right_inner).with_workers(nz(2));
    let out = run(&nested, Trail::default());
    assert!(out.is_ok());
    assert_eq!(out.state().as_slice(), ["l0", "l1", "l2", "r0", "r1", "r2"]);
    assert_eq!(left_max.load(Ordering::SeqCst), 1);
    assert_eq!(right_max.load(Ordering::SeqCst), 1);
}

/// Pins: `parallel.workers.panic_observation`, `parallel.workers.panic_no_global_hook`,
/// `parallel.workers.panic_global_hook_unused`
#[test]
fn parallel_workers_panic_observed_via_catch_unwind_without_global_hook() {
    #[derive(Debug, PartialEq, Eq)]
    struct CustomPayload(u32);

    // Verify no crate source file touches the process-global panic hook.
    for src in [
        include_str!("../src/error.rs"),
        include_str!("../src/graph.rs"),
        include_str!("../src/interp.rs"),
        include_str!("../src/lib.rs"),
        include_str!("../src/node.rs"),
        include_str!("../src/outcome.rs"),
        include_str!("../src/root.rs"),
        include_str!("../src/workers.rs"),
    ] {
        assert!(!src.contains("set_hook"), "process-global panic hook must not be used");
        assert!(!src.contains("take_hook"), "process-global panic hook must not be used");
        assert!(!src.contains("update_hook"), "process-global panic hook must not be used");
    }

    // Custom non-string panic payload propagates intact (not replaced by a generic join message).
    let boom: Node<Trail> = task("boom", |_: Trail| std::panic::panic_any(CustomPayload(42)));
    let workflow = step("ok").par(boom).with_workers(nz(2));

    let payload = catch_unwind(AssertUnwindSafe(|| run(&workflow, Trail::default()))).unwrap_err();
    assert_eq!(payload.downcast_ref::<CustomPayload>(), Some(&CustomPayload(42)));
}

/// Pins: `parallel.workers.panic_stop_signal`, `parallel.workers.panic_pending`
#[test]
fn parallel_workers_panic_stops_pending_branches_from_starting() {
    // Case 1 (workers = 1): panicking branch stops queued branches on the sole worker.
    let single_pending = Arc::new(AtomicUsize::new(0));
    let make_single_pending = |name: &'static str| {
        let pending_started = Arc::clone(&single_pending);
        task(name, move |mut t: Trail| {
            pending_started.fetch_add(1, Ordering::SeqCst);
            t.push(name);
            Outcome::Ok(t)
        })
    };

    let panicking: Node<Trail> = task("boom", |_: Trail| panic!("stop here"));
    let workflow =
        panicking.par(make_single_pending("p1")).par(make_single_pending("p2")).with_workers(nz(1));

    let err = catch_unwind(AssertUnwindSafe(|| run(&workflow, Trail::default()))).unwrap_err();
    assert_eq!(err.downcast_ref::<&str>(), Some(&"stop here"));
    assert_eq!(single_pending.load(Ordering::SeqCst), 0);

    // Case 2 (workers = 2): sibling worker observes the stop signal after b0's worker
    // has caught the panic, stored `stop = true`, and exited its thread, so when b1
    // completes and loops back it must NOT start pending branch b2.
    let both_started = Arc::new(Barrier::new(2));
    let (b0_exited_tx, b0_exited_rx) = mpsc::channel();
    let b0_exited_tx = Arc::new(Mutex::new(Some(b0_exited_tx)));
    let b0_exited_rx = Arc::new(Mutex::new(b0_exited_rx));
    let b1_finished = Arc::new(AtomicBool::new(false));
    let sibling_pending = Arc::new(AtomicUsize::new(0));

    let b0_barrier = Arc::clone(&both_started);
    let b0: Node<Trail> = task("b0", move |_: Trail| {
        b0_barrier.wait();
        signal_on_thread_exit(b0_exited_tx.lock().unwrap().take().unwrap());
        panic!("b0-stop");
    });

    let b1_barrier = Arc::clone(&both_started);
    let b1_done = Arc::clone(&b1_finished);
    let b1: Node<Trail> = task("b1", move |mut t: Trail| {
        b1_barrier.wait();
        // Happens-before edge: waits until worker 0 has fully exited after setting `stop`.
        b0_exited_rx.lock().unwrap().recv().unwrap();
        b1_done.store(true, Ordering::SeqCst);
        t.push("b1");
        Outcome::Ok(t)
    });

    let pending_counter = Arc::clone(&sibling_pending);
    let b2: Node<Trail> = task("b2", move |mut t: Trail| {
        pending_counter.fetch_add(1, Ordering::SeqCst);
        t.push("b2");
        Outcome::Ok(t)
    });

    let two_worker_wf = b0.par(b1).par(b2).with_workers(nz(2));
    let err2 =
        catch_unwind(AssertUnwindSafe(|| run(&two_worker_wf, Trail::default()))).unwrap_err();
    assert_eq!(err2.downcast_ref::<&str>(), Some(&"b0-stop"));
    assert!(b1_finished.load(Ordering::SeqCst), "started sibling branch b1 must complete");
    assert_eq!(
        sibling_pending.load(Ordering::SeqCst),
        0,
        "sibling worker must observe stop signal and not start pending branch b2"
    );
}

/// Pins: `parallel.workers.panic_started_boundary`, `parallel.workers.panic_join`
#[test]
fn parallel_workers_panic_joins_all_started_branches_before_propagation() {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let started_rx = Mutex::new(started_rx);
    let release_rx = Mutex::new(release_rx);
    let b1_finished = Arc::new(AtomicBool::new(false));
    let b2_started = Arc::new(AtomicUsize::new(0));
    let (b0_exited_tx, b0_exited_rx) = mpsc::channel();
    let b0_exited_tx = Mutex::new(Some(b0_exited_tx));

    // Branch 0 waits until branch 1 has definitely started, then panics.
    let b0: Node<Trail> = task("b0", move |_: Trail| {
        started_rx.lock().unwrap().recv().unwrap();
        signal_on_thread_exit(b0_exited_tx.lock().unwrap().take().unwrap());
        panic!("b0 panicked");
    });

    // Branch 1 starts before branch 0 panics (before stop signal), blocks on release,
    // and marks finished before returning.
    let b1_done_flag = Arc::clone(&b1_finished);
    let b1: Node<Trail> = task("b1", move |mut t: Trail| {
        started_tx.lock().unwrap().send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
        b1_done_flag.store(true, Ordering::SeqCst);
        t.push("b1");
        Outcome::Ok(t)
    });

    let b2_counter = Arc::clone(&b2_started);
    let b2: Node<Trail> = task("b2", move |mut t: Trail| {
        b2_counter.fetch_add(1, Ordering::SeqCst);
        t.push("b2");
        Outcome::Ok(t)
    });

    let workflow = b0.par(b1).par(b2).with_workers(nz(2));

    let (done_tx, done_rx) = mpsc::channel();
    let b1_finished_in_caller = Arc::clone(&b1_finished);

    let handle = thread::spawn(move || {
        let res = catch_unwind(AssertUnwindSafe(|| run(&workflow, Trail::default())));
        let saw_b1_finished = b1_finished_in_caller.load(Ordering::SeqCst);
        let _ = done_tx.send(saw_b1_finished);
        res
    });

    // Wait until worker 0 has completely caught the panic, set `stop`, and exited
    // while branch 1 is still blocked.
    b0_exited_rx.recv().unwrap();
    assert!(
        done_rx.try_recv().is_err(),
        "run must not propagate panic while started branch b1 is still running"
    );

    release_tx.send(()).unwrap();
    let saw_b1_finished = done_rx.recv().unwrap();
    assert!(saw_b1_finished, "started branch b1 must have finished before propagation");

    let err = handle.join().unwrap().unwrap_err();
    assert_eq!(err.downcast_ref::<&str>(), Some(&"b0 panicked"));
    assert_eq!(b2_started.load(Ordering::SeqCst), 0, "pending branch b2 must not start");
}

/// Pins: `parallel.workers.panic_first`, `parallel.workers.panic_no_occurrence_order`,
/// `parallel.workers.panic_no_lock_order`
#[test]
fn parallel_workers_panic_propagates_first_in_definition_order() {
    for limit in [Some(nz(2)), None] {
        let both_started = Arc::new(Barrier::new(2));
        let (b1_exited_tx, b1_exited_rx) = mpsc::channel();
        let b1_exited_rx = Arc::new(Mutex::new(b1_exited_rx));
        let b1_exited_tx = Arc::new(Mutex::new(Some(b1_exited_tx)));

        let b0_barrier = Arc::clone(&both_started);
        let b0: Node<Trail> = task("b0", move |_: Trail| {
            b0_barrier.wait();
            // Happens-before edge: waits until worker 1 has panicked, acquired
            // `queue.lock()`, recorded `panics[1]`, released the lock, and exited.
            // Thus b1 strictly precedes b0 in BOTH occurrence order and lock order.
            b1_exited_rx.lock().unwrap().recv().unwrap();
            panic!("def-0");
        });

        let b1_barrier = Arc::clone(&both_started);
        let b1: Node<Trail> = task("b1", move |_: Trail| {
            b1_barrier.wait();
            signal_on_thread_exit(b1_exited_tx.lock().unwrap().take().unwrap());
            panic!("def-1");
        });

        let mut workflow = b0.par(b1);
        if let Some(w) = limit {
            workflow = workflow.with_workers(w);
        }

        let err = catch_unwind(AssertUnwindSafe(|| run(&workflow, Trail::default()))).unwrap_err();
        assert_eq!(
            err.downcast_ref::<&str>(),
            Some(&"def-0"),
            "definition-order first panic must win (limit={limit:?})"
        );
    }
}

/// Pin: `parallel.workers.panic_cleanup`
#[test]
fn parallel_workers_panic_drops_unstarted_closures_and_completed_results_before_propagation() {
    struct DropLog {
        next_clone: AtomicUsize,
        events: Mutex<Vec<(String, bool)>>,
    }

    struct TrackedState {
        label: String,
        log: Arc<DropLog>,
    }

    impl Clone for TrackedState {
        fn clone(&self) -> Self {
            let id = self.log.next_clone.fetch_add(1, Ordering::SeqCst);
            Self { label: format!("clone-{id}"), log: Arc::clone(&self.log) }
        }
    }

    impl Drop for TrackedState {
        fn drop(&mut self) {
            self.log.events.lock().unwrap().push((self.label.clone(), thread::panicking()));
        }
    }

    impl Merge for TrackedState {
        fn merge(left: Self, _right: Self) -> Self {
            left
        }
    }

    struct CallerUnwindSpy(Arc<DropLog>);

    impl Drop for CallerUnwindSpy {
        fn drop(&mut self) {
            self.0.events.lock().unwrap().push(("caller-unwind".to_owned(), thread::panicking()));
        }
    }

    // With workers = 1:
    // - branch 0 (clone-0) completes Ok -> stored in results[0]
    // - branch 1 (clone-1) completes Err -> stored in results[1]
    // - branch 2 (clone-2) panics -> sets stop signal
    // - branch 3 (clone-3) never starts -> stays in pending closure
    let log = Arc::new(DropLog { next_clone: AtomicUsize::new(0), events: Mutex::new(Vec::new()) });
    let b3_ran = Arc::new(AtomicBool::new(false));

    let b0: Node<TrackedState> = task("b0", |mut s: TrackedState| {
        s.label = "completed-ok-0".to_owned();
        Outcome::Ok(s)
    });
    let b1: Node<TrackedState> = task("b1", |mut s: TrackedState| {
        s.label = "completed-err-1".to_owned();
        Outcome::Err(s, Error::new("e1", "failed"))
    });
    let b2: Node<TrackedState> = task("b2", |mut s: TrackedState| {
        s.label = "panicking-2".to_owned();
        panic!("boom-2");
    });
    let b3_ran_flag = Arc::clone(&b3_ran);
    let b3: Node<TrackedState> = task("b3", move |mut s: TrackedState| {
        b3_ran_flag.store(true, Ordering::SeqCst);
        s.label = "unexpected-3".to_owned();
        Outcome::Ok(s)
    });

    let workflow = b0.par(b1).par(b2).par(b3).with_workers(nz(1));
    let start = TrackedState { label: "initial".to_owned(), log: Arc::clone(&log) };

    let spy_log = Arc::clone(&log);
    let err = catch_unwind(AssertUnwindSafe(move || {
        let _spy = CallerUnwindSpy(spy_log);
        let _ = run(&workflow, start);
    }))
    .unwrap_err();

    assert_eq!(err.downcast_ref::<&str>(), Some(&"boom-2"));
    assert!(!b3_ran.load(Ordering::SeqCst), "unstarted branch 3 must not run");

    let events = log.events.lock().unwrap().clone();
    let pos = |label: &str| -> (usize, bool) {
        events
            .iter()
            .enumerate()
            .find_map(|(i, (l, p))| if l == label { Some((i, *p)) } else { None })
            .unwrap_or_else(|| panic!("missing drop event {label:?} in {events:?}"))
    };

    let (unwind_idx, unwind_panicking) = pos("caller-unwind");
    assert!(unwind_panicking, "caller-unwind spy must run during unwind");

    // Both completed results (Ok and Err) and the unstarted branch closure's state (clone-3)
    // must be dropped BEFORE propagation begins (panicking == false and before caller unwind).
    for expected in ["completed-ok-0", "completed-err-1", "clone-3"] {
        let (idx, was_panicking) = pos(expected);
        assert!(
            !was_panicking,
            "{expected} was dropped while already panicking; events={events:?}"
        );
        assert!(
            idx < unwind_idx,
            "{expected} (at {idx}) must drop before caller-unwind (at {unwind_idx}); events={events:?}"
        );
    }
}

/// Seam check: `Root`, `DryRun`, `Audit`, and `Retry` work through bounded `Node::Par`.
#[test]
fn parallel_workers_integrates_with_root_and_interpreters() {
    let workflow = step("a").par(step("b")).par(step("c")).with_workers(nz(2));

    // Root commits on Ok.
    let mut root = Root::new(Trail::default());
    assert!(root.run(&workflow).is_ok());
    assert_eq!(root.state().as_slice(), ["a", "b", "c"]);

    // Root does not commit on Err.
    let failing = step("a").par(fail("bad")).par(step("c")).with_workers(nz(1));
    let mut err_root = Root::new(Trail::default());
    let err_out = err_root.run(&failing);
    assert!(err_out.is_err());
    assert_eq!(err_out.state().as_slice(), ["bad"]);
    assert_eq!(err_root.state().0, Vec::<String>::new());

    // DryRun records all branches without executing bodies.
    let plan = DryRun::new();
    let dry_out = run_with(&workflow, Trail::default(), &plan);
    assert!(dry_out.is_ok());
    assert_eq!(dry_out.state().0, Vec::<String>::new());
    let mut planned = plan.planned();
    planned.sort();
    assert_eq!(planned, ["a", "b", "c"]);

    // Audit records every task while running under the worker limit.
    let audit = Audit::new(Real);
    let audit_out = run_with(&workflow, Trail::default(), &audit);
    assert_eq!(audit_out.state().as_slice(), ["a", "b", "c"]);
    let mut seen = audit.seen();
    seen.sort();
    assert_eq!(seen, ["a", "b", "c"]);

    // Retry retries flaky tasks inside bounded parallel branches.
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let flaky: Node<Trail> = task("flaky", move |mut t: Trail| {
        t.push("try");
        if counter.fetch_add(1, Ordering::SeqCst) < 1 {
            Outcome::Err(t, Error::new("flaky", "again"))
        } else {
            Outcome::Ok(t)
        }
    });
    let retry_wf = step("ok").par(flaky).par(step("done")).with_workers(nz(1));
    let retry_out = run_with(&retry_wf, Trail::default(), &Retry::without_backoff(Real, 3));
    assert!(retry_out.is_ok());
    assert_eq!(retry_out.state().as_slice(), ["ok", "try", "try", "done"]);
}
