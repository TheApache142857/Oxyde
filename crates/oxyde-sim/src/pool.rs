//! Worker threads started once and woken for each parallel step.
//!
//! Starting a thread costs some forty microseconds on Windows: a step of a solve run on
//! sixteen fresh threads (`std::thread::scope`) spent more on starting them than on its
//! work, about 550 µs against 150 µs for 300 000 values, and a generative design solve
//! makes some thirty such steps per conjugate gradient iteration. Here the threads start on
//! the first step (one per core but the caller's) and sleep on a condition variable between
//! steps; a step wakes them, the caller works alongside them, and it returns once every
//! task is done.
//!
//! Steps run one at a time: a second caller waits for the first's step to end, so solves on
//! several threads share the pool step by step. A step started from inside a task (on a
//! worker or on the caller while it works) runs its tasks itself, one after another, so it
//! never waits on the pool it is part of. A task that panics is caught where it runs and
//! its panic resumed on the caller once the step is over, as `std::thread::scope` does.

use std::any::Any;
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};

/// A step's work: task `i` of it. `'static` here only so the workers can hold it; see
/// [`run`].
type Job = dyn Fn(usize) + Sync;

/// The step's job with its lifetime erased.
#[derive(Clone, Copy)]
struct Erased(*const Job);

// SAFETY: the job is `Sync`, so calling it from several threads is sound, and the pointer
// is only dereferenced while `run`, which holds the borrow it was made from, waits.
unsafe impl Send for Erased {}

/// The step in hand.
struct Step {
    /// Bumped by each step, so a sleeping worker knows there is a new one.
    generation: u64,
    job: Option<Erased>,
    tasks: usize,
    /// The next task to hand out.
    next: usize,
    /// Tasks not finished yet, handed out or not.
    unfinished: usize,
    /// The first panic of a task, to resume on the caller.
    panic: Option<Box<dyn Any + Send>>,
}

struct Pool {
    step: Mutex<Step>,
    /// Workers wait on it for a new step; the caller waits on `finished` for its end.
    wake: Condvar,
    finished: Condvar,
    /// One step at a time.
    turn: Mutex<()>,
    workers: usize,
}

static POOL: OnceLock<Pool> = OnceLock::new();

thread_local! {
    /// Whether this thread is running a task of a step: a step it starts runs serially.
    static INSIDE: Cell<bool> = const { Cell::new(false) };
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Tasks run outside the locks, and nothing that holds one can panic: a poisoned lock
    // still holds consistent data.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pool {
    fn start() -> Pool {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let mut pool = Pool {
            step: Mutex::new(Step {
                generation: 0,
                job: None,
                tasks: 0,
                next: 0,
                unfinished: 0,
                panic: None,
            }),
            wake: Condvar::new(),
            finished: Condvar::new(),
            turn: Mutex::new(()),
            workers: 0,
        };
        // The workers are started below, once the pool has its place in `POOL`.
        pool.workers = cores.saturating_sub(1);
        pool
    }

    /// Takes tasks of the step in hand and runs them until none is left to take.
    fn work(&self) {
        loop {
            let (job, task) = {
                let mut step = lock(&self.step);
                let Some(job) = step.job else {
                    return;
                };
                if step.next >= step.tasks {
                    return;
                }
                let task = step.next;
                step.next += 1;
                (job, task)
            };
            // SAFETY: the task was handed out before `unfinished` reached zero, and `run`
            // does not return (nor end the borrow the pointer comes from) until it has.
            let result = catch_unwind(AssertUnwindSafe(|| unsafe { (*job.0)(task) }));
            let mut step = lock(&self.step);
            if let Err(panic) = result {
                step.panic.get_or_insert(panic);
            }
            step.unfinished -= 1;
            if step.unfinished == 0 {
                self.finished.notify_all();
            }
        }
    }
}

fn worker(pool: &'static Pool) {
    INSIDE.with(|i| i.set(true));
    let mut seen = 0;
    loop {
        {
            let mut step = lock(&pool.step);
            while step.generation == seen {
                step = pool.wake.wait(step).unwrap_or_else(PoisonError::into_inner);
            }
            seen = step.generation;
        }
        pool.work();
    }
}

fn pool() -> &'static Pool {
    let mut fresh = false;
    let pool = POOL.get_or_init(|| {
        fresh = true;
        Pool::start()
    });
    if fresh {
        for k in 0..pool.workers {
            // A worker that cannot start is simply missing: the others and the caller take
            // its share, tasks being handed out as they are asked for.
            let _ = std::thread::Builder::new()
                .name(format!("oxyde-sim {k}"))
                .spawn(move || worker(pool));
        }
    }
    pool
}

/// Runs `job(0)`, …, `job(tasks − 1)` on the pool's threads and the caller's, and returns
/// once every one is done. Each task should depend on its index only, so that what they
/// make does not depend on which thread runs which.
pub(crate) fn run(tasks: usize, job: &(dyn Fn(usize) + Sync)) {
    if tasks <= 1 || INSIDE.with(Cell::get) {
        for task in 0..tasks {
            job(task);
        }
        return;
    }
    let pool = pool();
    if pool.workers == 0 {
        for task in 0..tasks {
            job(task);
        }
        return;
    }
    let _turn = lock(&pool.turn);
    let short: *const (dyn Fn(usize) + Sync + '_) = job;
    // SAFETY: only the lifetime changes. The workers call the job for tasks handed out
    // before `unfinished` reaches zero; this function waits for that and takes the job back
    // out of the step before it returns, so no call outlives the borrow of `job`.
    let erased = Erased(unsafe {
        std::mem::transmute::<*const (dyn Fn(usize) + Sync + '_), *const Job>(short)
    });
    {
        let mut step = lock(&pool.step);
        step.generation += 1;
        step.job = Some(erased);
        step.tasks = tasks;
        step.next = 0;
        step.unfinished = tasks;
        step.panic = None;
    }
    pool.wake.notify_all();
    INSIDE.with(|i| i.set(true));
    pool.work();
    INSIDE.with(|i| i.set(false));
    let panic = {
        let mut step = lock(&pool.step);
        while step.unfinished > 0 {
            step = pool
                .finished
                .wait(step)
                .unwrap_or_else(PoisonError::into_inner);
        }
        step.job = None;
        step.panic.take()
    };
    if let Some(panic) = panic {
        resume_unwind(panic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_task_runs_once_and_steps_from_tasks_run_serially() {
        for tasks in [0, 1, 2, 7, 64, 1000] {
            let counts: Vec<AtomicUsize> = (0..tasks).map(|_| AtomicUsize::new(0)).collect();
            run(tasks, &|i| {
                counts[i].fetch_add(1, Ordering::Relaxed);
                // A step inside a task: run where it is, without waiting on the pool.
                let inner = AtomicUsize::new(0);
                run(3, &|_| {
                    inner.fetch_add(1, Ordering::Relaxed);
                });
                assert_eq!(inner.load(Ordering::Relaxed), 3);
            });
            assert!(
                counts.iter().all(|c| c.load(Ordering::Relaxed) == 1),
                "{tasks}"
            );
        }
    }

    #[test]
    fn steps_from_several_threads_share_the_pool() {
        let total = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..200 {
                        run(16, &|i| {
                            total.fetch_add(i, Ordering::Relaxed);
                        });
                    }
                });
            }
        });
        assert_eq!(
            total.load(Ordering::Relaxed),
            4 * 200 * (0..16).sum::<usize>()
        );
    }

    #[test]
    fn a_panicking_task_is_resumed_on_the_caller_and_the_pool_goes_on() {
        let caught = catch_unwind(|| {
            run(8, &|i| {
                if i == 5 {
                    std::panic::panic_any("task 5");
                }
            });
        });
        assert!(caught.is_err());
        let done = AtomicUsize::new(0);
        run(8, &|_| {
            done.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(done.load(Ordering::Relaxed), 8);
    }
}
