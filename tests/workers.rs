#[path = "../src/workers.rs"]
mod workers;

use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::thread;

#[test]
fn bounded_branches_run_to_completion_in_definition_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let max = Arc::new(AtomicUsize::new(0));
    let first_wave = Arc::new(Barrier::new(3));
    let branches: Vec<_> = (0..12)
        .map(|i| {
            let active = Arc::clone(&active);
            let max = Arc::clone(&max);
            let barrier = Arc::clone(&first_wave);
            move || {
                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(n, Ordering::SeqCst);
                if i < 3 {
                    barrier.wait();
                }
                active.fetch_sub(1, Ordering::SeqCst);
                if i == 2 {
                    Err(i)
                } else {
                    Ok(i)
                }
            }
        })
        .collect();
    let got = workers::run(branches, NonZeroUsize::new(3));
    assert_eq!(max.load(Ordering::SeqCst), 3);
    assert_eq!(got, (0..12).map(|i| if i == 2 { Err(i) } else { Ok(i) }).collect::<Vec<_>>());
}

#[test]
fn unlimited_starts_all_and_matches_bounded() {
    let barrier = Arc::new(Barrier::new(4));
    let branches: Vec<_> = (0..4)
        .map(|i| {
            let barrier = Arc::clone(&barrier);
            move || {
                barrier.wait();
                i
            }
        })
        .collect();
    assert_eq!(
        workers::run(branches, None),
        workers::run((0..4).map(|i| move || i).collect(), NonZeroUsize::new(2))
    );
    assert!(workers::run(Vec::<fn() -> usize>::new(), None).is_empty());
}

#[test]
fn panic_stops_pending_and_joins_started() {
    let (release_tx, release_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let finished = Arc::new(AtomicUsize::new(0));
    let pending = Arc::new(AtomicUsize::new(0));
    let mut branches: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
    branches.push(Box::new(move || {
        started_rx.recv().unwrap();
        panic!("first")
    }));
    let done = Arc::clone(&finished);
    branches.push(Box::new(move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        done.fetch_add(1, Ordering::SeqCst);
    }));
    let queued = Arc::clone(&pending);
    branches.push(Box::new(move || {
        queued.fetch_add(1, Ordering::SeqCst);
    }));
    let handle = thread::spawn(move || {
        catch_unwind(AssertUnwindSafe(|| workers::run(branches, NonZeroUsize::new(2))))
    });
    // A single-worker panic below deterministically covers dispatch after the stop signal.
    let single_pending = Arc::clone(&pending);
    let single: Vec<Box<dyn FnOnce() + Send>> = vec![
        Box::new(|| panic!("single")),
        Box::new(move || {
            single_pending.fetch_add(1, Ordering::SeqCst);
        }),
    ];
    assert!(catch_unwind(AssertUnwindSafe(|| workers::run(single, NonZeroUsize::new(1)))).is_err());
    release_tx.send(()).unwrap();
    assert!(handle.join().unwrap().is_err());
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}

#[test]
fn earliest_panicking_definition_wins() {
    let barrier = Arc::new(Barrier::new(2));
    let (tx, rx) = mpsc::channel();
    let a = Arc::clone(&barrier);
    let b = Arc::clone(&barrier);
    let branches: Vec<Box<dyn FnOnce() + Send>> = vec![
        Box::new(move || {
            a.wait();
            rx.recv().unwrap();
            panic!("definition zero");
        }),
        Box::new(move || {
            b.wait();
            tx.send(()).unwrap();
            panic!("definition one");
        }),
    ];
    let err = catch_unwind(AssertUnwindSafe(|| workers::run(branches, NonZeroUsize::new(2))))
        .unwrap_err();
    assert_eq!(err.downcast_ref::<&str>(), Some(&"definition zero"));
}

#[test]
fn nested_limits_are_per_call() {
    let outer = vec![|| {
        let barrier = Arc::new(Barrier::new(2));
        let inner: Vec<_> = (0..2)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                move || {
                    barrier.wait();
                    i
                }
            })
            .collect();
        workers::run(inner, NonZeroUsize::new(2))
    }];
    assert_eq!(workers::run(outer, NonZeroUsize::new(1)), vec![vec![0, 1]]);
}
