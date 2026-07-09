//! Test-only inspection helpers for scoped environment state.

use super::ThreadState;
#[cfg(all(test, feature = "loom-tests"))]
use super::{EnvLockOps, ThreadStateCore};

#[cfg(all(test, feature = "loom-tests"))]
impl<L: EnvLockOps> ThreadStateCore<L> {
    /// Return the current per-thread recursive scope depth.
    pub(crate) const fn depth(&self) -> usize { self.depth }

    /// Report whether all tracked scope entries have been restored.
    pub(crate) fn is_stack_empty(&self) -> bool { self.stack.is_empty() }

    /// Report whether this thread currently owns the backend lock.
    pub(crate) const fn has_lock(&self) -> bool { self.lock.is_some() }

    /// Inspect the held lock guard during Loom-only model assertions.
    pub(crate) fn with_lock_guard<R>(&self, inspect: impl FnOnce(&L::Guard) -> R) -> R {
        let Some(guard) = self.lock.as_ref() else {
            panic!("ScopedEnv should hold the mutex during active inspection");
        };
        inspect(guard)
    }
}

#[cfg(test)]
impl ThreadState {
    /// Return the current test-visible recursive scope depth.
    pub const fn depth(&self) -> usize { self.inner.depth }

    /// Report whether test-visible state has no tracked scopes.
    pub fn is_stack_empty(&self) -> bool { self.inner.stack.is_empty() }

    /// Report whether test-visible state currently holds `ENV_LOCK`.
    pub const fn has_lock(&self) -> bool { self.inner.lock.is_some() }
}
