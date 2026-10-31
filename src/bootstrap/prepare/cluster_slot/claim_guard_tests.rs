//! Tests that a slot claim holds the claim guard until its slot is locked.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use rstest::rstest;

use super::{
    ClusterSlot,
    claim_slot,
    claim_slot_with_hooks,
    tests::{Parent, parent},
};
use crate::error::BootstrapResult;

/// The point at which a test pauses the first claim.
#[derive(Clone, Copy, Debug)]
enum PausePoint {
    /// Inside the dead-slot sweep.
    Sweep,
    /// After the new slot's lock file is created and locked.
    SlotCreation,
}

/// A one-shot callback a claim runs at a [`PausePoint`].
type Hook = Box<dyn FnOnce() + Send>;

/// A first claim parked at a [`PausePoint`], and the means to resume it.
struct PausedClaim {
    claim: std::thread::JoinHandle<BootstrapResult<ClusterSlot>>,
    resume: std::sync::mpsc::Sender<()>,
}

/// Starts a claim that stops at `point` until resumed, and returns once it is
/// parked there.
fn start_paused_claim(parent: Utf8PathBuf, point: PausePoint) -> PausedClaim {
    let (paused_tx, paused_rx) = std::sync::mpsc::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel();
    let pause = move || {
        paused_tx.send(()).expect("report the pause");
        resume_rx.recv().expect("wait for the test to resume");
    };
    let (on_sweep, on_created): (Hook, Hook) = match point {
        PausePoint::Sweep => (Box::new(pause), Box::new(|| {})),
        PausePoint::SlotCreation => (Box::new(|| {}), Box::new(pause)),
    };
    let claim = std::thread::spawn(move || {
        claim_slot_with_hooks(
            &parent,
            move |_| {
                on_sweep();
                Ok(0)
            },
            on_created,
        )
    });
    paused_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the first claim reaches the pause point");
    PausedClaim { claim, resume }
}

/// Starts a second claim and returns once it is running, with a channel that
/// reports whether it succeeded when it finishes.
fn start_second_claim(
    parent: Utf8PathBuf,
) -> (std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<bool>) {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let claim = std::thread::spawn(move || {
        started_tx.send(()).expect("report claim start");
        let claimed = claim_slot(&parent);
        finished_tx
            .send(claimed.is_ok())
            .expect("report claim result");
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the second claim starts");
    (claim, finished_rx)
}

/// Another bootstrap cannot claim while the first claim holds the guard, from
/// the sweep through the creation and locking of its slot.
#[rstest]
#[case::sweep(PausePoint::Sweep)]
#[case::slot_creation(PausePoint::SlotCreation)]
fn a_claim_holds_the_guard_until_its_slot_is_locked(
    parent: Result<Parent>,
    #[case] pause: PausePoint,
) {
    let dir = parent.expect("data parent");
    let first = start_paused_claim(dir.dir.clone(), pause);
    let (second, second_finished) = start_second_claim(dir.dir.clone());
    assert!(
        second_finished
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_err(),
        "a second claim must wait until sweeping and slot creation finish ({pause:?})"
    );

    first.resume.send(()).expect("resume the first claim");
    first
        .claim
        .join()
        .expect("join the first claim")
        .expect("the first claim succeeds");
    assert!(
        second_finished
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the second claim proceeds")
    );
    second.join().expect("join the second claim");
}
