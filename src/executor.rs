use crossbeam_deque::{Injector, Stealer, Worker};
use futures::Future;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    thread,
};

use crate::reactor::Reactor;

lazy_static! {
    pub static ref RUNTIME: Reactor = Reactor::new(1024, 2, 100);
}

use {
    futures::{
        future::{BoxFuture, FutureExt},
        task::{waker_ref, ArcWake},
    },
    std::sync::mpsc::{sync_channel, Receiver, SyncSender},
};

const num_threads: usize = 5;

pub struct Executor {
    task_receiver: Vec<Worker<Arc<Task>>>,
    task_stealer: Vec<Stealer<Arc<Task>>>,
    global: Arc<Injector<Arc<Task>>>,
}

/// `Spawner` spawns new futures onto the task channel.
#[derive(Clone)]
pub struct Spawner {
    task_sender: Arc<Injector<Arc<Task>>>,
}

static curr_worker: AtomicUsize = AtomicUsize::new(0);

/// A future that can reschedule itself to be polled by an `Executor`.
struct Task {
    /// In-progress future that should be pushed to completion.
    ///
    /// The `Mutex` is not necessary for correctness, since we only have
    /// one thread executing tasks at once. However, Rust isn't smart
    /// enough to know that `future` is only mutated from one thread,
    /// so we need to use the `Mutex` to prove thread-safety. A production
    /// executor would not need this, and could use `UnsafeCell` instead.
    future: Mutex<Option<BoxFuture<'static, ()>>>,

    /// Handle to place the task itself back onto the task queue.
    task_sender: Arc<Injector<Arc<Task>>>,
}

pub fn new_executor_and_spawner() -> (Executor, Spawner) {
    // Maximum number of tasks to allow queueing in the channel at once.
    // This is just to make `sync_channel` happy, and wouldn't be present in
    // a real executor.
    const MAX_QUEUED_TASKS: usize = 10_000;

    let task_sender = Arc::new(Injector::<Arc<Task>>::new());
    let mut task_receiver: Vec<Worker<Arc<Task>>> = vec![];
    let mut task_stealer: Vec<Stealer<Arc<Task>>> = vec![];
    for _ in 0..num_threads {
        //     let (sender, receiver) = sync_channel(MAX_QUEUED_TASKS);
        let worker = Worker::new_lifo();
        task_stealer.push(worker.stealer());
        task_receiver.push(worker);
    }
    (
        Executor {
            task_receiver,
            task_stealer,
            global: task_sender.clone(),
        },
        Spawner { task_sender },
    )
}
// ANCHOR_END: executor_decl

// ANCHOR: spawn_fn
impl Spawner {
    pub fn spawn(&self, future: impl Future<Output = ()> + 'static + Send) {
        // println!("spawning");
        let future = future.boxed();
        let curr = curr_worker.fetch_add(1, Ordering::Relaxed) % num_threads;
        let task = Arc::new(Task {
            future: Mutex::new(Some(future)),
            task_sender: self.task_sender.clone(),
        });
        self.task_sender.push(task);
    }
}
// ANCHOR_END: spawn_fn

// ANCHOR: arcwake_for_task
impl ArcWake for Task {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        // Implement `wake` by sending this task back onto the task channel
        // so that it will be polled again by the executor.
        let cloned = arc_self.clone();
        arc_self.task_sender.push(cloned);
    }
}
// ANCHOR_END: arcwake_for_task

// ANCHOR: executor_run
impl Executor {
    pub fn run(self) {
        let global = self.global.clone();
        let task_stealer = self.task_stealer;
        for ready_queue in self.task_receiver {
            let global = global.clone();
            let task_stealer = task_stealer.clone();
            thread::spawn(move || {
                loop {
                    if let Some(task) = find_task(&ready_queue, &global, &task_stealer) {
                        let mut future_slot = task.future.lock().unwrap();
                        if let Some(mut future) = future_slot.take() {
                            // Create a `LocalWaker` from the task itself
                            let waker = waker_ref(&task);
                            let context = &mut Context::from_waker(&*waker);
                            // `BoxFuture<T>` is a type alias for
                            // `Pin<Box<dyn Future<Output = T> + Send + 'static>>`.
                            // We can get a `Pin<&mut dyn Future + Send + 'static>`
                            // from it by calling the `Pin::as_mut` method.
                            if let Poll::Pending = future.as_mut().poll(context) {
                                // We're not done processing the future, so put it
                                // back in its task to be run again in the future.
                                *future_slot = Some(future);
                            }
                        }
                    } else {
                        thread::sleep(core::time::Duration::from_millis(5));
                    }
                }
            });
        }
        loop {}
    }
}

fn find_task<T>(local: &Worker<T>, global: &Injector<T>, stealers: &[Stealer<T>]) -> Option<T> {
    // Pop a task from the local queue, if not empty.
    local.pop().or_else(|| {
        // Otherwise, we need to look for a task elsewhere.
        std::iter::repeat_with(|| {
            // Try stealing a batch of tasks from the global queue.
            global
                .steal_batch_and_pop(local)
                // Or try stealing a task from one of the other threads.
                .or_else(|| stealers.iter().map(|s| s.steal()).collect())
        })
        // Loop while no task was stolen and any steal operation needs to be retried.
        .find(|s| !s.is_retry())
        // Extract the stolen task, if there is one.
        .and_then(|s| s.success())
    })
}
