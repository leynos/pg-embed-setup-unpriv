//! Shared singleton state-machine core.

/// State machine for fallible lazy singleton initialisation.
pub(super) enum SharedInitState<T, C> {
    /// Not yet initialised.
    Uninitialised,
    /// Successfully initialised with the cached value.
    Initialised(T),
    /// Initialisation failed; stores the cached failure details.
    Failed(C),
}

/// Result type returned by a first-attempt initialiser.
pub(super) type InitialiserResult<T, C, E> = Result<T, (C, E)>;

/// Resolves a shared singleton state, initialising it at most once.
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
