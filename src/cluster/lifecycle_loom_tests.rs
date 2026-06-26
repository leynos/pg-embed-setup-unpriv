//! Loom checks for template database lifecycle coordination.

use super::lifecycle_template::{
    TemplateCreationOps, TemplateLockOps, ensure_template_exists_with_lock,
};
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;
use std::collections::{BTreeMap, BTreeSet};

struct LoomTemplateLocks {
    locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl LoomTemplateLocks {
    fn new() -> Self {
        Self {
            locks: Mutex::new(BTreeMap::new()),
        }
    }
}

impl TemplateLockOps for LoomTemplateLocks {
    fn with_template_lock<R>(&self, template_name: &str, operation: impl FnOnce() -> R) -> R {
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locks
                .entry(template_name.to_owned())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation()
    }
}

#[derive(Clone)]
struct TemplateHarness {
    locks: Arc<LoomTemplateLocks>,
    created: Arc<Mutex<BTreeSet<String>>>,
    setup_count: Arc<AtomicUsize>,
}

impl TemplateHarness {
    fn new() -> Self {
        Self {
            locks: Arc::new(LoomTemplateLocks::new()),
            created: Arc::new(Mutex::new(BTreeSet::new())),
            setup_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn ensure_template(&self, template_name: &'static str) {
        let created_for_exists = Arc::clone(&self.created);
        let created_for_create = Arc::clone(&self.created);
        let created_for_drop = Arc::clone(&self.created);
        let setup_count = Arc::clone(&self.setup_count);
        let result = ensure_template_exists_with_lock(
            self.locks.as_ref(),
            template_name,
            TemplateCreationOps {
                database_exists: move || {
                    let created = created_for_exists
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    Ok(created.contains(template_name))
                },
                create_database: move || {
                    let mut created = created_for_create
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    assert!(
                        created.insert(template_name.to_owned()),
                        "template should be created at most once"
                    );
                    Ok(())
                },
                drop_database: move || {
                    let mut created = created_for_drop
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    created.remove(template_name);
                    Ok(())
                },
                setup_fn: move || {
                    setup_count.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            },
        );
        assert!(result.is_ok(), "template setup should succeed");
    }

    fn created_count(&self) -> usize {
        let created = self
            .created
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        created.len()
    }
}

fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut builder = loom::model::Builder::new();
    builder.max_threads = 3;
    builder.max_branches = 64;
    builder.preemption_bound = Some(3);
    builder.check(f);
}

#[test]
#[ignore = "requires Loom model checking"]
fn same_template_setup_runs_once_under_race() {
    run_loom_model(|| {
        let harness = TemplateHarness::new();
        let first = harness.clone();
        let second = harness.clone();

        let first_thread = thread::spawn(move || first.ensure_template("template"));
        let second_thread = thread::spawn(move || second.ensure_template("template"));

        assert!(first_thread.join().is_ok(), "first thread should join");
        assert!(second_thread.join().is_ok(), "second thread should join");
        assert_eq!(harness.created_count(), 1);
        assert_eq!(harness.setup_count.load(Ordering::SeqCst), 1);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn different_template_setups_do_not_deadlock() {
    run_loom_model(|| {
        let harness = TemplateHarness::new();
        let first = harness.clone();
        let second = harness.clone();

        let first_thread = thread::spawn(move || first.ensure_template("template_a"));
        let second_thread = thread::spawn(move || second.ensure_template("template_b"));

        assert!(first_thread.join().is_ok(), "first thread should join");
        assert!(second_thread.join().is_ok(), "second thread should join");
        assert_eq!(harness.created_count(), 2);
        assert_eq!(harness.setup_count.load(Ordering::SeqCst), 2);
    });
}
