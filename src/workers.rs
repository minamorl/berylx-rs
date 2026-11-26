//! Per-call bounded execution of independent branches.

use std::any::Any;
use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

type Panic = Box<dyn Any + Send + 'static>;

struct Queue<F, R> {
    pending: VecDeque<(usize, F)>,
    results: Vec<Option<R>>,
    panics: Vec<Option<Panic>>,
}

/// Run branches with a per-call limit, returning results in definition order.
#[allow(dead_code)]
pub fn run<F, R>(branches: Vec<F>, limit: Option<NonZeroUsize>) -> Vec<R>
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    let count = branches.len();
    let workers = limit.map_or(count, NonZeroUsize::get).min(count);
    let stop = AtomicBool::new(false);
    let queue = Mutex::new(Queue {
        pending: branches.into_iter().enumerate().collect(),
        results: std::iter::repeat_with(|| None).take(count).collect(),
        panics: std::iter::repeat_with(|| None).take(count).collect(),
    });
    thread::scope(|scope| {
        for _ in 0..workers {
            let queue = &queue;
            let stop = &stop;
            scope.spawn(move || loop {
                let job = {
                    let mut state = queue.lock().unwrap_or_else(PoisonError::into_inner);
                    if stop.load(Ordering::SeqCst) {
                        None
                    } else {
                        state.pending.pop_front()
                    }
                };
                let Some((index, branch)) = job else { break };
                let outcome = catch_unwind(AssertUnwindSafe(branch));
                if outcome.is_err() {
                    stop.store(true, Ordering::SeqCst);
                }
                let mut state = queue.lock().unwrap_or_else(PoisonError::into_inner);
                match outcome {
                    Ok(value) => state.results[index] = Some(value),
                    Err(payload) => {
                        state.panics[index] = Some(payload);
                        break;
                    }
                }
            });
        }
    });
    let mut state = queue.into_inner().unwrap_or_else(PoisonError::into_inner);
    if stop.load(Ordering::SeqCst) {
        let payload = state.panics.iter_mut().find_map(Option::take).expect("panic recorded");
        drop(state.pending);
        drop(state.results);
        for other in state.panics.into_iter().flatten() {
            if let Err(drop_panic) = catch_unwind(AssertUnwindSafe(|| drop(other))) {
                std::mem::forget(drop_panic);
            }
        }
        resume_unwind(payload);
    }
    state.results.into_iter().map(|v| v.expect("branch completed")).collect()
}
