use super::*;

#[test]
fn the_restart_backoff_starts_at_250_ms_and_doubles() {
    let waits: Vec<_> = (1..MAX_RESTART_LAUNCHES).map(backoff).collect();

    assert_eq!(
        waits,
        [250, 500, 1000, 2000].map(Duration::from_millis),
        "a restart waits before each of its launches after the first"
    );
}
