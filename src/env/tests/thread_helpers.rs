//! Thread coordination helpers for cross-thread environment tests.
//!
//! Provides the drop guards and spawn routines used by
//! `serializes_env_across_threads` to exercise cross-thread ordering.

use std::{
    env,
    ffi::{OsStr, OsString},
    panic,
    sync::{Arc, Barrier, mpsc},
    thread,
};

use thiserror::Error;

use super::{ENV_LOCK, ScopedEnv, remove_env_var_unlocked, set_env_var_unlocked};

/// Coordination failure raised by a scoped-environment guard thread.
///
/// Whitaker treats `#[serial]`-wrapped tests and their helpers as production
/// code, because the attribute macros are gone by the time the lint sees HIR.
/// The guard threads therefore report channel failures instead of calling
/// `expect`, and `serializes_env_across_threads` propagates them.
#[derive(Debug, Error)]
pub(super) enum GuardThreadError {
    /// A coordination signal never arrived because the sender was dropped.
    #[error("the {signal} signal was not received")]
    Receive {
        /// Name of the signal that was awaited.
        signal: &'static str,
        /// Underlying channel failure.
        #[source]
        source: mpsc::RecvError,
    },
    /// A coordination signal could not be delivered because the receiver was
    /// dropped. The payload is not retained, since it is either a unit or the
    /// observed environment value the test has already stopped waiting for.
    #[error("the {signal} signal could not be sent")]
    Send {
        /// Name of the signal that could not be delivered.
        signal: &'static str,
    },
}

/// Receive a coordination signal, naming it if the sender has gone away.
fn receive_signal(
    receiver: &mpsc::Receiver<()>,
    signal: &'static str,
) -> Result<(), GuardThreadError> {
    receiver
        .recv()
        .map_err(|source| GuardThreadError::Receive { signal, source })
}

/// Send a coordination signal, naming it if the receiver has gone away.
fn send_signal<T>(
    sender: &mpsc::Sender<T>,
    value: T,
    signal: &'static str,
) -> Result<(), GuardThreadError> {
    sender
        .send(value)
        .map_err(|_| GuardThreadError::Send { signal })
}

/// Join a guard thread, re-raising its panic and surfacing its failure.
///
/// A panic inside the thread is resumed on the joining thread so the original
/// message and backtrace survive; an orderly failure is returned instead.
///
/// # Examples
///
/// ```ignore
/// join_guard_thread(spawn_outer_guard_thread(key, channels))?;
/// ```
pub(super) fn join_guard_thread(
    handle: thread::JoinHandle<Result<(), GuardThreadError>>,
) -> Result<(), GuardThreadError> {
    match handle.join() {
        Ok(result) => result,
        Err(payload) => panic::resume_unwind(payload),
    }
}

/// Sends a unit on drop via `mpsc::Sender` and ignores send errors.
pub(super) struct ReleaseOnDrop {
    pub(super) sender: Option<mpsc::Sender<()>>,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            // Ignore send errors - receiver may have dropped after a test failure.
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Receiver may have dropped after a test failure."
            )]
            let _ = sender.send(());
        }
    }
}

/// Restores or removes a named env var while holding `ENV_LOCK`, delegating to
/// the unlocked helpers that perform the underlying mutations.
///
/// # Panic safety
///
/// `RestoreEnv::drop` acquires `ENV_LOCK`, so callers must ensure the lock is
/// not held when a `RestoreEnv` is dropped. In `serializes_env_across_threads`
/// this is enforced by calling `assert_env_lock_released()` before letting the
/// `RestoreEnv` go out of scope. Alternatively, if spawned threads panic due to
/// channel closure, the panic unwinds and drops their guards, releasing
/// `ENV_LOCK` before `RestoreEnv::drop` runs.
pub(super) struct RestoreEnv {
    pub(super) key: String,
    pub(super) original: Option<OsString>,
}

impl Drop for RestoreEnv {
    fn drop(&mut self) {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &self.original {
            Some(value) => set_env_var_unlocked(OsStr::new(&self.key), value.as_os_str()),
            None => remove_env_var_unlocked(OsStr::new(&self.key)),
        }
    }
}

/// Channels and synchronization primitives for the outer guard thread.
pub(super) struct ThreadAChannels {
    /// Barrier used to co-ordinate with other threads.
    pub(super) barrier: Arc<Barrier>,
    /// Sender used to signal readiness after applying the scoped env.
    pub(super) ready_tx: mpsc::Sender<()>,
    /// Receiver used to wait for release before dropping the guard.
    pub(super) release_rx: mpsc::Receiver<()>,
    /// Sender used to signal completion after the guard drops.
    pub(super) done_tx: mpsc::Sender<()>,
}

/// Channels used by thread B to coordinate acquisition, report state, and
/// signal completion.
pub(super) struct ThreadBChannels {
    /// Signals when thread A has instructed thread B to begin.
    pub(super) start_rx: mpsc::Receiver<()>,
    /// Notifies the main thread that thread B is attempting to acquire the lock.
    pub(super) attempt_tx: mpsc::Sender<()>,
    /// Reports the environment value observed after acquiring the guard.
    pub(super) acquired_tx: mpsc::Sender<Option<String>>,
    /// Signals that thread B has completed its work.
    pub(super) done_tx: mpsc::Sender<()>,
}

/// Spawn the inner-guard thread that blocks on the mutex, reports the value,
/// and signals completion.
///
/// # Errors
///
/// The thread returns `GuardThreadError` when any coordination channel closes
/// before its signal is exchanged.
pub(super) fn spawn_inner_guard_thread(
    key: String,
    channels: ThreadBChannels,
) -> thread::JoinHandle<Result<(), GuardThreadError>> {
    let ThreadBChannels {
        start_rx,
        attempt_tx,
        acquired_tx,
        done_tx,
    } = channels;
    thread::spawn(move || {
        receive_signal(&start_rx, "start")?;
        send_signal(&attempt_tx, (), "attempt")?;
        let guard = ScopedEnv::apply(&[(key.clone(), Some(String::from("two")))]);

        let value = env::var(&key).ok();
        send_signal(&acquired_tx, value, "acquired value")?;
        drop(guard);
        send_signal(&done_tx, (), "completion")
    })
}

/// Spawn a thread that applies a scoped environment variable and waits on
/// synchronization primitives.
///
/// # Parameters
///
/// - `key`: Environment key string to set while the scoped guard is held.
/// - `channels`: `ThreadAChannels` containing the coordination primitives:
///   - `barrier`: `Arc<Barrier>` used to co-ordinate with other threads.
///   - `ready_tx`: `mpsc::Sender<()>` used to signal readiness after applying.
///   - `release_rx`: `mpsc::Receiver<()>` used to wait for release before dropping the guard.
///   - `done_tx`: `mpsc::Sender<()>` used to signal completion after the guard is dropped.
///
/// # Behaviour
///
/// Calls `ScopedEnv::apply` to set the env var to "one", sends the ready
/// signal, waits on the barrier, blocks on `release_rx`, then drops the guard
/// to restore the environment and signals completion.
///
/// # Errors
///
/// The thread returns `GuardThreadError` if the ready signal cannot be sent,
/// if the release signal is not received, or if the completion signal cannot
/// be sent. Join it with `join_guard_thread` to surface that failure.
///
/// # Returns
///
/// Returns a `thread::JoinHandle<Result<(), GuardThreadError>>` for the
/// spawned thread.
///
/// # Examples
///
/// ```ignore
/// let barrier = Arc::new(Barrier::new(2));
/// let (ready_tx, _ready_rx) = mpsc::channel();
/// let (_release_tx, release_rx) = mpsc::channel();
/// let (done_tx, _done_rx) = mpsc::channel();
///
/// let handle = spawn_outer_guard_thread(
///     String::from("THREAD_SCOPE_TEST"),
///     ThreadAChannels {
///         barrier: Arc::clone(&barrier),
///         ready_tx,
///         release_rx,
///         done_tx,
///     },
/// );
///
/// barrier.wait();
/// join_guard_thread(handle)?;
/// ```
pub(super) fn spawn_outer_guard_thread(
    key: String,
    channels: ThreadAChannels,
) -> thread::JoinHandle<Result<(), GuardThreadError>> {
    let ThreadAChannels {
        barrier,
        ready_tx,
        release_rx,
        done_tx,
    } = channels;
    thread::spawn(move || {
        let guard = ScopedEnv::apply(&[(key, Some(String::from("one")))]);

        send_signal(&ready_tx, (), "ready")?;
        barrier.wait();
        receive_signal(&release_rx, "release")?;
        drop(guard);
        send_signal(&done_tx, (), "completion")
    })
}
