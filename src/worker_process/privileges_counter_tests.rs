//! Deterministic tests for the privilege-drop guard counter.

use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};

use super::decrement_counter;

fn decrement_after_both_load(counter: &AtomicUsize, both_loaded: &Barrier) {
    decrement_counter(counter, || {
        both_loaded.wait();
    });
}

#[test]
fn concurrent_decrements_retry_without_losing_the_remaining_guard() {
    // A third, held guard lets us verify that both concurrent releases leave
    // an exemption in force until the final release.
    let counter = AtomicUsize::new(3);
    let both_loaded = Barrier::new(2);

    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| decrement_after_both_load(&counter, &both_loaded));
        }
    });

    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "the held guard remains active"
    );
    decrement_counter(&counter, || {});
    assert_eq!(
        counter.load(Ordering::SeqCst),
        0,
        "the final release clears the counter"
    );
}
