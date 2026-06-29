//! Thread-local state and mutex management for scoped environment guards.

use crate::observability::LOG_TARGET;
use std::env;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

pub(crate) trait EnvLockOps {
    type Guard: 'static;

    fn lock_env_mutex() -> Self::Guard;
    fn ensure_lock_is_clean();
    fn var_os(guard: &Self::Guard, key: &OsString) -> Option<OsString>;
    fn set_var(guard: &mut Self::Guard, key: &OsString, value: OsString);
    fn remove_var(guard: &mut Self::Guard, key: &OsString);
}

#[derive(Debug)]
pub(crate) struct StdEnvLock;

impl EnvLockOps for StdEnvLock {
    type Guard = MutexGuard<'static, ()>;

    fn lock_env_mutex() -> Self::Guard {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ensure_lock_is_clean() {
        if ENV_LOCK.is_poisoned() {
            tracing::warn!(
                target: LOG_TARGET,
                "ENV_LOCK was poisoned; clearing poison and proceeding"
            );
            ENV_LOCK.clear_poison();
        }
    }

    fn var_os(_guard: &Self::Guard, key: &OsString) -> Option<OsString> {
        env::var_os(key)
    }

    fn set_var(_guard: &mut Self::Guard, key: &OsString, value: OsString) {
        unsafe {
            // SAFETY: `ENV_LOCK` serialises changes. Drop restores recorded
            // values before releasing the lock.
            env::set_var(key, value);
        }
    }

    fn remove_var(_guard: &mut Self::Guard, key: &OsString) {
        unsafe {
            // SAFETY: `ENV_LOCK` serialises changes. Drop restores recorded
            // values before releasing the lock.
            env::remove_var(key);
        }
    }
}

#[derive(Debug)]
pub(crate) struct GuardState {
    pub(crate) saved: Vec<(OsString, Option<OsString>)>,
    pub(crate) finished: bool,
}

#[derive(Debug)]
pub(crate) struct ThreadStateCore<L: EnvLockOps> {
    depth: usize,
    lock: Option<L::Guard>,
    stack: Vec<GuardState>,
}

#[derive(Debug)]
pub(crate) struct ThreadState {
    inner: ThreadStateCore<StdEnvLock>,
}

impl ThreadState {
    pub const fn new() -> Self {
        Self {
            inner: ThreadStateCore::new(),
        }
    }

    pub fn enter_scope<I>(&mut self, vars: I) -> usize
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        self.inner.enter_scope(vars)
    }

    pub fn exit_scope(&mut self, index: usize) {
        self.inner.exit_scope(index);
    }
}

#[cfg(all(test, feature = "loom-tests"))]
pub(crate) type ThreadStateInner<L> = ThreadStateCore<L>;

impl<L: EnvLockOps> ThreadStateCore<L> {
    pub const fn new() -> Self {
        Self {
            depth: 0,
            lock: None,
            stack: Vec::new(),
        }
    }

    pub fn enter_scope<I>(&mut self, vars: I) -> usize
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        let vars: Vec<_> = vars.into_iter().collect();
        for (key, _) in &vars {
            Self::validate_env_key(key);
        }

        self.acquire_lock_if_needed();

        self.depth += 1;

        let saved = self.apply_env_vars(vars);

        let index = self.stack.len();
        self.stack.push(GuardState {
            saved,
            finished: false,
        });
        index
    }

    pub fn exit_scope(&mut self, index: usize) {
        if self.depth == 0 {
            self.force_restore_and_reset("ScopedEnv drop without matching apply", None);
            return;
        }
        self.depth -= 1;

        if !self.finish_scope(index) {
            return;
        }

        if self.depth == 0 {
            self.release_outermost_lock();
        }
    }

    fn acquire_lock_if_needed(&mut self) {
        if self.depth > 0 {
            return;
        }

        assert!(
            self.lock.is_none(),
            "ScopedEnv depth desynchronised: mutex still held",
        );
        L::ensure_lock_is_clean();
        let guard = L::lock_env_mutex();
        self.lock = Some(guard);
    }

    fn apply_env_vars<I>(&mut self, vars: I) -> Vec<(OsString, Option<OsString>)>
    where
        I: IntoIterator<Item = (OsString, Option<OsString>)>,
    {
        let guard = self.guard_mut("ScopedEnv must hold the mutex before mutating the environment");
        let mut saved = Vec::new();
        for (key, new_value) in vars {
            let previous = Self::apply_single_var(guard, &key, new_value);
            saved.push((key, previous));
        }
        saved
    }

    fn validate_env_key(key: &OsString) {
        assert!(
            !key.is_empty(),
            "ScopedEnv received an empty environment variable name"
        );
        assert!(
            !Self::contains_equals(key),
            "ScopedEnv received an environment variable name containing '='"
        );
    }

    #[cfg(unix)]
    fn contains_equals(key: &OsString) -> bool {
        use std::os::unix::ffi::OsStrExt;

        key.as_os_str().as_bytes().contains(&b'=')
    }

    #[cfg(windows)]
    fn contains_equals(key: &OsString) -> bool {
        use std::os::windows::ffi::OsStrExt;

        key.as_os_str()
            .encode_wide()
            .any(|value| value == u16::from(b'='))
    }

    #[cfg(not(any(unix, windows)))]
    fn contains_equals(key: &OsString) -> bool {
        key.to_string_lossy().contains('=')
    }

    fn apply_single_var(
        guard: &mut L::Guard,
        key: &OsString,
        new_value: Option<OsString>,
    ) -> Option<OsString> {
        debug_assert!(
            !key.is_empty() && !Self::contains_equals(key),
            "invalid env var name: {key:?}"
        );
        let previous = L::var_os(guard, key);
        match new_value {
            Some(value) => L::set_var(guard, key, value),
            None => L::remove_var(guard, key),
        }
        previous
    }

    fn finish_scope(&mut self, index: usize) -> bool {
        {
            let Some(state) = self.stack.get_mut(index) else {
                self.force_restore_and_reset("ScopedEnv finished out of order", Some(index));
                return false;
            };
            if state.finished {
                self.force_restore_and_reset("ScopedEnv finished twice", Some(index));
                return false;
            }
            state.finished = true;
        }

        self.restore_finished_scopes();
        true
    }

    fn restore_finished_scopes(&mut self) {
        if self.stack.last().is_some_and(|state| state.finished) {
            self.ensure_lock_for_restore();
        }
        while let Some(guard_state) = self.stack.pop() {
            if !guard_state.finished {
                self.stack.push(guard_state);
                break;
            }
            let guard = self.guard_mut(
                "ScopedEnv must hold the mutex before restoring finished environment scopes",
            );
            restore_saved::<L>(guard, guard_state.saved);
        }
    }

    fn release_outermost_lock(&mut self) {
        debug_assert!(
            self.stack.is_empty(),
            "ScopedEnv stack must be empty once recursion depth reaches zero",
        );
        if let Some(guard) = self.lock.take() {
            drop(guard);
        } else {
            debug_assert!(false, "ScopedEnv mutex guard missing at depth zero");
        }
    }

    fn force_restore_and_reset(&mut self, reason: &str, index: Option<usize>) {
        self.log_corruption(reason, index);
        if self.stack.is_empty() {
            if self.lock.is_some() {
                self.reset_depth_and_unlock();
            } else {
                self.depth = 0;
            }
            return;
        }
        self.ensure_lock_for_restore();
        self.restore_all_scopes();
        self.reset_depth_and_unlock();
    }

    fn log_corruption(&self, reason: &str, index: Option<usize>) {
        let depth = self.depth;
        let stack_len = self.stack.len();
        let has_lock = self.lock.is_some();
        tracing::error!(
            target: LOG_TARGET,
            depth,
            stack_len,
            has_lock,
            index = ?index,
            "{reason}; restoring environment and resetting state"
        );
    }

    fn ensure_lock_for_restore(&mut self) {
        if self.lock.is_none() {
            L::ensure_lock_is_clean();
            self.lock = Some(L::lock_env_mutex());
        }
    }

    fn restore_all_scopes(&mut self) {
        if self.stack.is_empty() {
            return;
        }
        self.ensure_lock_for_restore();
        while let Some(state) = self.stack.pop() {
            let guard = self.guard_mut("ScopedEnv must hold the mutex before restoring all scopes");
            restore_saved::<L>(guard, state.saved);
        }
    }

    fn reset_depth_and_unlock(&mut self) {
        self.depth = 0;
        if let Some(guard) = self.lock.take() {
            drop(guard);
        }
    }

    fn guard_mut(&mut self, context: &str) -> &mut L::Guard {
        let Some(guard) = self.lock.as_mut() else {
            panic!("{context}");
        };
        guard
    }
}

#[cfg(all(test, feature = "loom-tests"))]
impl<L: EnvLockOps> ThreadStateCore<L> {
    pub(crate) const fn depth(&self) -> usize {
        self.depth
    }

    pub(crate) fn is_stack_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub(crate) const fn has_lock(&self) -> bool {
        self.lock.is_some()
    }

    pub(crate) fn with_lock_guard<R>(&self, inspect: impl FnOnce(&L::Guard) -> R) -> R {
        let Some(guard) = self.lock.as_ref() else {
            panic!("ScopedEnv should hold the mutex during active inspection");
        };
        inspect(guard)
    }
}

#[cfg(test)]
impl ThreadState {
    pub const fn depth(&self) -> usize {
        self.inner.depth
    }

    pub fn is_stack_empty(&self) -> bool {
        self.inner.stack.is_empty()
    }

    pub const fn has_lock(&self) -> bool {
        self.inner.lock.is_some()
    }
}

fn restore_saved<L: EnvLockOps>(guard: &mut L::Guard, saved: Vec<(OsString, Option<OsString>)>) {
    for (key, value) in saved.into_iter().rev() {
        match value {
            Some(previous) => L::set_var(guard, &key, previous),
            None => L::remove_var(guard, &key),
        }
    }
}
