//! Shared singleton state-machine core.
//!
//! This module owns the fallible lazy-initialization state machine shared by
//! `shared_singleton.rs` and the Loom model tests. Keeping the transition logic
//! here gives runtime callers and test-only concurrency models the same
//! behaviour without coupling either side to the other's locking primitive or
//! storage strategy.

/// State machine for fallible lazy singleton initialization.
pub(super) enum SharedInitState<T, C> {
    /// Not yet initialized.
    Uninitialised,
    /// Successfully initialized with the cached value.
    Initialised(T),
    /// Initialization failed; stores the cached failure details.
    Failed(C),
}

/// Result type returned by a first-attempt initializer.
pub(super) type InitialiserResult<T, C, E> = Result<T, (C, E)>;

/// Resolves a shared singleton state, initializing it at most once.
pub(super) fn get_or_try_init_shared<T, C, E, Init, CachedError>(
    state: &mut SharedInitState<T, C>,
    init: Init,
    cached_error: CachedError,
) -> Result<T, E>
where
    T: Copy,
    Init: FnOnce() -> InitialiserResult<T, C, E>,
    CachedError: FnOnce(&C) -> E,
{
    match state {
        SharedInitState::Initialised(value) => Ok(*value),
        SharedInitState::Failed(cached) => Err(cached_error(cached)),
        SharedInitState::Uninitialised => match init() {
            Ok(value) => {
                *state = SharedInitState::Initialised(value);
                Ok(value)
            }
            Err((cached, err)) => {
                *state = SharedInitState::Failed(cached);
                Err(err)
            }
        },
    }
}
