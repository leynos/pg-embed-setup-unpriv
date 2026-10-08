//! Each watcher decision reaches a consumer's recorder as one bounded
//! outcome. The recorder is process-wide, so these carry the same
//! `serial` key as the crate's other metric tests, and assert containment
//! because the cleanup tests call the watcher without that key.

use std::sync::{Arc, Mutex, PoisonError};

use color_eyre::eyre::{Result, ensure};
use serial_test::serial;

use super::{Slot, release_watcher_for_test};
use crate::{
    bootstrap::prepare::orphan_watch::{release_watcher, watch_slot_owner, watcher_pid},
    observability::{
        Metric,
        MetricsRecorder,
        OrphanWatcherOutcomeMetric as Outcome,
        install_metrics_recorder,
    },
};

#[derive(Default)]
struct Collected(Mutex<Vec<Metric>>);

impl MetricsRecorder for Collected {
    fn record(&self, metric: Metric) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(metric);
    }
}

fn collected(body: impl FnOnce()) -> Vec<Metric> {
    let recorder = Arc::new(Collected::default());
    let guard = install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);
    body();
    drop(guard);
    let seen = recorder.0.lock().unwrap_or_else(PoisonError::into_inner);
    seen.clone()
}

/// A directory that is not a slot is counted as such.
#[test]
#[serial(metrics_recorder)]
fn a_directory_that_is_not_a_slot_is_counted() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let seen = collected(|| {
        let _watching = watch_slot_owner(dir.path());
    });
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::NotASlot)),
        "{seen:?}"
    );
    Ok(())
}

/// A slot is counted as spawned, and releasing it as released.
#[test]
#[serial(metrics_recorder)]
fn spawning_and_releasing_a_watcher_are_counted() -> Result<()> {
    let slot = Slot::new()?;
    let seen = collected(|| {
        let _watching = watch_slot_owner(&slot.data_dir);
        let _released = release_watcher(&slot.data_dir);
    });
    release_watcher_for_test(&slot.data_dir);
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::Spawned)),
        "{seen:?}"
    );
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::Released)),
        "{seen:?}"
    );
    Ok(())
}

/// A watcher whose teardown fails is not counted as released: the failure is
/// counted as `ReleaseFailed` instead, because the watcher may still be waiting.
///
/// The watcher is killed and reaped behind the registry's back, so the
/// registry's own `kill` finds no such process and `wait` no such child.
#[test]
#[serial(metrics_recorder)]
fn a_watcher_that_cannot_be_ended_is_not_counted_as_released() -> Result<()> {
    use nix::{
        sys::{
            signal::{Signal, kill},
            wait::waitpid,
        },
        unistd::Pid,
    };

    let slot = Slot::new()?;
    ensure!(watch_slot_owner(&slot.data_dir), "the watcher must spawn");
    let pid = Pid::from_raw(i32::try_from(
        watcher_pid(&slot.data_dir).ok_or_else(|| color_eyre::eyre::eyre!("no watcher"))?,
    )?);
    kill(pid, Signal::SIGKILL)?;
    waitpid(pid, None)?;
    let seen = collected(|| {
        let _released = release_watcher(&slot.data_dir);
    });
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::ReleaseFailed)),
        "{seen:?}"
    );
    ensure!(
        !seen.contains(&Metric::OrphanWatcher(Outcome::Released)),
        "a failed teardown must not be counted as released: {seen:?}"
    );
    Ok(())
}

/// Replacing a watcher that cannot be ended is counted as `ReleaseFailed` too,
/// and the new watcher is still counted as spawned.
#[test]
#[serial(metrics_recorder)]
fn a_replaced_watcher_that_cannot_be_ended_is_counted() -> Result<()> {
    use nix::{
        sys::{
            signal::{Signal, kill},
            wait::waitpid,
        },
        unistd::Pid,
    };

    let slot = Slot::new()?;
    ensure!(watch_slot_owner(&slot.data_dir), "the watcher must spawn");
    let pid = Pid::from_raw(i32::try_from(
        watcher_pid(&slot.data_dir).ok_or_else(|| color_eyre::eyre::eyre!("no watcher"))?,
    )?);
    kill(pid, Signal::SIGKILL)?;
    waitpid(pid, None)?;
    let seen = collected(|| {
        let _watching = watch_slot_owner(&slot.data_dir);
    });
    release_watcher_for_test(&slot.data_dir);
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::ReleaseFailed)),
        "{seen:?}"
    );
    ensure!(
        seen.contains(&Metric::OrphanWatcher(Outcome::Spawned)),
        "{seen:?}"
    );
    Ok(())
}
