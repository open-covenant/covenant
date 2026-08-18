//! A settlement operator under bond, across three windows.
//!
//! ```text
//! cargo run -p covenant-bond --example bonded_operator
//! ```
//!
//! The operator runs clean, then degrades, then tries to tidy the record. The
//! point of the third window is that tidying it is what stops the claim
//! verifying: the slash is derived from the log, so editing the log after the
//! fact does not soften the slash, it invalidates the operator's own copy of
//! the evidence.

use covenant_bond::{Bond, Obligation, ObservationLog, Outcome, Sla, SlashClaim, SlashPolicy};

const OPERATOR: &str = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";

fn main() {
    // 5000 units at risk, 20 percent of it per breached window.
    let bond = Bond::new(OPERATOR, 5_000_000_000).expect("bond");
    let policy = SlashPolicy::new(2_000).expect("policy");
    // Settle inside 2s, one miss forgiven, judged over a 60s window.
    let sla = Sla::new(2_000, 1, 60_000).expect("sla");

    println!("operator {OPERATOR}");
    println!(
        "bond     {} units, {} bps per breached window",
        bond.posted, policy.slash_bps
    );
    println!(
        "sla      settle within {}ms, {} miss tolerated per {}ms\n",
        sla.settle_within_ms, sla.tolerated_misses, sla.window_ms
    );

    let mut log = ObservationLog::default();

    // Window one: everything settles well inside the promise.
    for i in 0..5u64 {
        let at = 1_000 + i * 1_000;
        log.append(settled(&format!("w1-{i}"), at, 400));
    }
    report(&log, &bond, policy, sla, 60_000, "window 1: healthy");

    // Window two: upstream degrades. Two time out and one lands late, which is
    // past the single miss the SLA forgives.
    log.append(timed_out("w2-0", 61_000));
    log.append(settled("w2-1", 62_000, 5_000));
    log.append(timed_out("w2-2", 63_000));
    let claim = report(&log, &bond, policy, sla, 120_000, "window 2: degraded");

    let Some(claim) = claim else {
        panic!("window 2 was supposed to breach");
    };

    // Window three: the operator rewrites the two timeouts as prompt
    // settlements and re-presents the log.
    let mut tidied = ObservationLog::new(
        log.entries()
            .iter()
            .map(|o| match o.outcome {
                Outcome::TimedOut => settled(&o.id, o.accepted_at_ms, 400),
                _ => o.clone(),
            })
            .collect(),
    );
    tidied.append(settled("w3-0", 121_000, 400));

    println!("window 3: the operator tidies the record");
    println!("  claimed root  {}…", &claim.observation_root[..16]);
    println!("  tidied root   {}…", &tidied.root().expect("root")[..16]);
    println!(
        "  claim against the tidied log: {}",
        verdict(claim.verifies_against(&tidied))
    );
    println!(
        "  claim against the real log:   {}",
        verdict(claim.verifies_against(&log))
    );

    assert!(
        !claim.verifies_against(&tidied),
        "an edited log must not satisfy the claim"
    );
    assert!(
        claim.verifies_against(&log),
        "the claim must still hold against the record it was built from"
    );

    println!("\nA slash follows the record. Editing the record does not soften it.");
}

fn report(
    log: &ObservationLog,
    bond: &Bond,
    policy: SlashPolicy,
    sla: Sla,
    now_ms: u64,
    label: &str,
) -> Option<SlashClaim> {
    let claim = SlashClaim::for_breach(bond, policy, log, sla, now_ms).expect("assess");
    let assessment = covenant_bond::assess(log, sla, now_ms);

    println!("{label}");
    println!(
        "  observed {} obligation(s), {} miss(es)",
        assessment.observed, assessment.misses
    );
    match &claim {
        None => println!("  no claim: within the promise\n"),
        Some(c) => {
            println!("  BREACH   {:?}", assessment.missed_ids);
            println!("  slash    {} of {} units", c.amount, c.bond_posted);
            println!("  root     {}…", &c.observation_root[..16]);
            println!("  verifies {}\n", verdict(c.verifies_against(log)));
        }
    }
    claim
}

fn settled(id: &str, at_ms: u64, took_ms: u64) -> Obligation {
    Obligation::new(
        id,
        at_ms,
        Outcome::Settled {
            at_ms: at_ms + took_ms,
        },
    )
    .expect("obligation")
}

fn timed_out(id: &str, at_ms: u64) -> Obligation {
    Obligation::new(id, at_ms, Outcome::TimedOut).expect("obligation")
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "yes"
    } else {
        "no"
    }
}
