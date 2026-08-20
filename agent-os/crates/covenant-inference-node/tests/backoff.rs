use std::time::Duration;

use covenant_inference_node::backoff::Backoff;

#[test]
fn ceiling_is_monotonic_and_capped() {
    let backoff = Backoff::new(Duration::from_millis(250), Duration::from_secs(30));
    let mut previous = Duration::ZERO;
    for attempt in 0..40 {
        let ceiling = backoff.ceiling(attempt);
        assert!(ceiling >= previous, "ceiling must not decrease");
        assert!(
            ceiling <= Duration::from_secs(30),
            "ceiling must never exceed the cap"
        );
        previous = ceiling;
    }
    // Far past the doubling range it pins to the cap and stays there.
    assert_eq!(backoff.ceiling(40), Duration::from_secs(30));
    assert_eq!(backoff.ceiling(u32::MAX), Duration::from_secs(30));
}

#[test]
fn samples_stay_within_the_jitter_band() {
    let backoff = Backoff::new(Duration::from_millis(250), Duration::from_secs(30));
    for attempt in 0..40 {
        let ceiling = backoff.ceiling(attempt);
        let floor = ceiling / 2;
        for entropy in [0, 1, 7, 1_000, u64::MAX / 2, u64::MAX] {
            let delay = backoff.sample(attempt, entropy);
            assert!(delay >= floor, "sample dropped below the jitter floor");
            assert!(delay <= ceiling, "sample rose above the ceiling");
        }
    }
}

#[test]
fn zero_entropy_yields_the_floor() {
    let backoff = Backoff::new(Duration::from_millis(200), Duration::from_secs(10));
    let ceiling = backoff.ceiling(3);
    assert_eq!(backoff.sample(3, 0), ceiling / 2);
}
