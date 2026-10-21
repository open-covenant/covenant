use super::*;
use crate::binding::parse_events;
use crate::vendor_types::TdxEventEntry;

const OS_HASH: [u8; 32] = [0x61; 32];
const PROFILE: TdxRuntimeProfile = TdxRuntimeProfile::Dstack061 {
    os_image_hash: OS_HASH,
};

fn runtime(name: &str, payload: &[u8], version: u32) -> TdxEvent {
    let mut event = TdxEvent {
        imr: 3,
        event_type: DSTACK_RUNTIME_EVENT_TYPE,
        name: name.into(),
        digest: Vec::new(),
        payload: payload.to_vec(),
        version,
        preimage: None,
    };
    if version == 2 {
        let preimage = serde_jcs::to_vec(&serde_json::json!({
            "name": name, "payload": hex::encode(payload), "type": DSTACK_RUNTIME_EVENT_TYPE,
        }))
        .unwrap();
        event.digest = Sha384::digest(&preimage).to_vec();
        event.preimage = Some(preimage);
    }
    event
}

fn boot(version: u32) -> Vec<TdxEvent> {
    vec![
        runtime("system-preparing", b"", version),
        runtime("app-id", &[0x11; 20], version),
        runtime("compose-hash", &[0x22; 32], version),
        runtime("instance-id", &[0x33; 20], version),
        runtime("os-image-hash", &OS_HASH, version),
        runtime("key-provider", br#"{"name":"kms","id":"abcd"}"#, version),
        runtime("system-ready", b"", version),
    ]
}

fn ephemeral_boot(version: u32) -> Vec<TdxEvent> {
    let mut events = boot(version);
    events[5] = runtime("key-provider", br#"{"name":"none","id":""}"#, version);
    events.insert(6, runtime("storage-fs", b"ext4", version));
    events.insert(7, runtime("storage-encrypted", b"1", version));
    events.remove(4); // The pinned KMS-free path never emits os-image-hash.
    events
}

fn verify_ephemeral(
    report: &TdReport,
    events: &[TdxEvent],
) -> Result<TdxRuntimeBindings, VerificationError> {
    verify_tdx_event_log_for_key_provider(report, events, PROFILE, DstackKeyProvider::EphemeralNone)
}

#[test]
fn managed_and_ephemeral_profiles_accept_only_their_own_key_provider() {
    for version in [1, 2] {
        let managed = boot(version);
        let ephemeral = ephemeral_boot(version);
        assert!(verify_tdx_event_log(&report(&managed), &managed, PROFILE).is_ok());
        assert!(verify_ephemeral(&report(&managed), &managed).is_err());
        assert!(verify_ephemeral(&report(&ephemeral), &ephemeral).is_ok());
        assert!(verify_tdx_event_log(&report(&ephemeral), &ephemeral, PROFILE).is_err());
        assert!(
            verify_tdx_event_log_for_key_provider(
                &report(&ephemeral),
                &ephemeral,
                TdxRuntimeProfile::LegacyMrKms,
                DstackKeyProvider::EphemeralNone
            )
            .is_err()
        );
    }
}

#[test]
fn ephemeral_profile_rejects_keyed_and_malformed_provider_identities() {
    for payload in [
        br#"{"name":"kms","id":"abcd"}"#.as_slice(),
        br#"{"name":"kms","id":""}"#.as_slice(),
        br#"{"name":"local","id":"abcd"}"#.as_slice(),
        br#"{"name":"local","id":""}"#.as_slice(),
        br#"{"name":"tpm","id":"abcd"}"#.as_slice(),
        br#"{"name":"tpm","id":""}"#.as_slice(),
        br#"{"name":"none","id":"00"}"#.as_slice(),
        br#"{"name":"none","id":null}"#.as_slice(),
        br#"{"name":"none"}"#.as_slice(),
        br#"{"id":""}"#.as_slice(),
        br#"{"name":"none","id":"","extra":false}"#.as_slice(),
        br#"{"name":"none","name":"none","id":""}"#.as_slice(),
    ] {
        let mut events = ephemeral_boot(2);
        events[4] = runtime("key-provider", payload, 2);
        assert!(verify_ephemeral(&report(&events), &events).is_err());
    }
}

#[test]
fn ephemeral_profile_preserves_complete_boot_and_os_identity_checks() {
    let original = ephemeral_boot(2);
    for index in 1..original.len() {
        let mut missing = original.clone();
        missing.remove(index);
        assert!(verify_ephemeral(&report(&missing), &missing).is_err());
        let mut duplicate = original.clone();
        duplicate.insert(index, duplicate[index].clone());
        assert!(verify_ephemeral(&report(&duplicate), &duplicate).is_err());
    }
    let mut post_ready = original.clone();
    let provider = post_ready.remove(4);
    post_ready.push(provider);
    assert!(verify_ephemeral(&report(&post_ready), &post_ready).is_err());
    let mut legacy = original;
    legacy.insert(5, runtime("mr-kms", &[0; 32], 2));
    assert!(verify_ephemeral(&report(&legacy), &legacy).is_err());
}

#[test]
fn none_boot_requires_absent_kms_os_event_while_managed_boot_still_requires_it() {
    for version in [1, 2] {
        for payload in [OS_HASH, [0x62; 32]] {
            let original = ephemeral_boot(version);
            for index in [4, original.len()] {
                let mut injected = original.clone();
                injected.insert(index, runtime("os-image-hash", &payload, version));
                assert!(verify_ephemeral(&report(&injected), &injected).is_err());
            }
        }
        let mut managed = boot(version);
        managed.remove(4);
        assert!(verify_tdx_event_log(&report(&managed), &managed, PROFILE).is_err());
    }
}

#[test]
fn ephemeral_profile_requires_pre_ready_encrypted_ext4_runtime_storage() {
    for (name, value) in [
        ("storage-encrypted", b"0".as_slice()),
        ("storage-fs", b"zfs".as_slice()),
    ] {
        let mut events = ephemeral_boot(2);
        let index = events.iter().position(|event| event.name == name).unwrap();
        events[index] = runtime(name, value, 2);
        assert!(verify_ephemeral(&report(&events), &events).is_err());
    }
    for name in ["storage-encrypted", "storage-fs"] {
        let mut events = ephemeral_boot(2);
        let index = events.iter().position(|event| event.name == name).unwrap();
        let event = events.remove(index);
        events.push(event);
        assert!(verify_ephemeral(&report(&events), &events).is_err());
    }
}

#[test]
fn ephemeral_profile_replays_the_entire_log_and_rejects_relabelled_events() {
    let original = ephemeral_boot(2);
    let quoted = report(&original);
    let mut reordered = original.clone();
    reordered.swap(1, 2);
    let mut truncated = original.clone();
    truncated.pop();
    let mut changed = original.clone();
    changed[2] = runtime("compose-hash", &[0x23; 32], 2);
    let mut hidden = original;
    hidden[5].event_type = 0;
    for events in [reordered, truncated, changed, hidden] {
        assert!(verify_ephemeral(&quoted, &events).is_err());
    }
}

fn report(events: &[TdxEvent]) -> TdReport {
    let mut folds = [[0u8; 48]; 4];
    for event in events {
        let mut hasher = Sha384::new();
        hasher.update(folds[event.imr as usize]);
        hasher.update(effective_digest(event).unwrap());
        folds[event.imr as usize] = hasher.finalize().into();
    }
    TdReport {
        mr_td: [0; 48],
        rtmr0: folds[0],
        rtmr1: folds[1],
        rtmr2: folds[2],
        rtmr3: folds[3],
        report_data: [0; 64],
    }
}

#[test]
fn v2_matches_pinned_dstack_canonical_preimage_vector() {
    // Pinned dstack 7f143148 runtime_events.rs canonical_event_json_v2:
    // the payload is lowercase hex and object keys are JCS-sorted.
    let event = runtime("compose-hash", &[0xab, 0xcd], 2);
    assert_eq!(
        event.preimage.as_deref().unwrap(),
        br#"{"name":"compose-hash","payload":"abcd","type":134217729}"#
    );
    assert_eq!(
        hex::encode(effective_digest(&event).unwrap()),
        "9dde1673f544faa94b921825209593ddde08d0baf9cd6990175dffaf3081f1bb10311d56de189491feb33842a050ad7b"
    );
}

#[test]
fn v2_requires_matching_digest_and_canonical_public_field_preimage() {
    let valid = runtime("compose-hash", &[0xab, 0xcd], 2);
    let mut missing_preimage = valid.clone();
    missing_preimage.preimage = None;
    let mut missing_digest = valid.clone();
    missing_digest.digest.clear();
    let mut changed_name = valid.clone();
    changed_name.name.push('x');
    let mut changed_payload = valid.clone();
    changed_payload.payload[0] ^= 1;
    let mut changed_digest = valid.clone();
    changed_digest.digest[0] ^= 1;
    let mut reordered_json = valid.clone();
    let noncanonical = br#"{"payload":"abcd","name":"compose-hash","type":134217729}"#;
    reordered_json.preimage = Some(noncanonical.to_vec());
    reordered_json.digest = Sha384::digest(noncanonical).to_vec();
    let mut unknown_version = valid.clone();
    unknown_version.version = 3;
    let mut relabeled_version = valid;
    relabeled_version.version = 1;
    for event in [
        missing_preimage,
        missing_digest,
        changed_name,
        changed_payload,
        changed_digest,
        reordered_json,
        unknown_version,
        relabeled_version,
    ] {
        assert!(effective_digest(&event).is_err());
    }
}

#[test]
fn modern_boot_accepts_v1_v2_and_mixed_logs_with_application_events() {
    for version in [1, 2] {
        let mut events = boot(version);
        // Mixed logs are explicitly supported upstream; version isn't OS identity.
        events.push(runtime("application-event", b"hello", 3 - version));
        let binding = verify_tdx_event_log(&report(&events), &events, PROFILE).unwrap();
        assert_eq!(binding.compose_hash, [0x22; 32]);
        assert_eq!(binding.instance_id, [0x33; 20]);
        assert!(
            verify_tdx_event_log(&report(&events), &events, TdxRuntimeProfile::LegacyMrKms,)
                .is_err()
        );
    }
}

#[test]
fn modern_boot_requires_complete_unique_pre_ready_identities() {
    let original = boot(2);
    for index in 1..=6 {
        let mut missing = original.clone();
        missing.remove(index);
        assert!(verify_tdx_event_log(&report(&missing), &missing, PROFILE).is_err());

        let mut duplicate = original.clone();
        duplicate.insert(index, duplicate[index].clone());
        assert!(verify_tdx_event_log(&report(&duplicate), &duplicate, PROFILE).is_err());
    }
    // Even a correctly replayed post-ready claim cannot fill a missing boot identity.
    let mut post_ready = original;
    let compose = post_ready.remove(2);
    post_ready.push(compose);
    assert!(verify_tdx_event_log(&report(&post_ready), &post_ready, PROFILE).is_err());
}

#[test]
fn modern_boot_rejects_bad_identity_lengths_os_or_key_provider() {
    for (name, payload) in [
        ("compose-hash", vec![0x22; 31]),
        ("app-id", vec![0x11; 19]),
        ("instance-id", vec![]),
        ("instance-id", vec![0x33; 21]),
        ("os-image-hash", vec![0x62; 32]),
        ("key-provider", br#"{"name":"none","id":""}"#.to_vec()),
        ("key-provider", br#"{"name":"kms","id":"not-hex"}"#.to_vec()),
        (
            "key-provider",
            br#"{"name":"kms","id":"ab","name":"none"}"#.to_vec(),
        ),
        ("system-ready", vec![1]),
    ] {
        let mut events = boot(2);
        let index = events.iter().position(|e| e.name == name).unwrap();
        events[index] = runtime(name, &payload, 2);
        assert!(
            verify_tdx_event_log(&report(&events), &events, PROFILE).is_err(),
            "{name}"
        );
    }
    let mut events = boot(2);
    events.insert(5, runtime("mr-kms", &[0; 32], 2));
    assert!(verify_tdx_event_log(&report(&events), &events, PROFILE).is_err());
}

#[test]
fn v2_replay_rejects_reordering_truncation_and_wrong_register() {
    let original = boot(2);
    let quoted = report(&original);
    let mut reordered = original.clone();
    reordered.swap(1, 2);
    let mut truncated = original.clone();
    truncated.pop();
    let mut wrong_register = original;
    wrong_register[2].imr = 2;
    for events in [reordered, truncated, wrong_register] {
        assert!(verify_tdx_event_log(&quoted, &events, PROFILE).is_err());
    }
}

#[test]
fn opaque_imr3_relabel_cannot_hide_boot_identity_and_ready_boundary() {
    for version in [1, 2] {
        // An app can extend its own compose-hash and system-ready events after
        // boot. Those extensions are authentic, but cannot replace boot identity.
        let mut authentic = boot(version);
        authentic.push(runtime("compose-hash", &[0x99; 32], version));
        authentic.push(runtime("system-ready", b"", version));
        let quoted = report(&authentic);
        assert!(verify_tdx_event_log(&quoted, &authentic, PROFILE).is_err());
        let mut supplied = authentic.clone();
        // Hide the original compose and ready events behind opaque firmware
        // types, preserving the exact digests the TD really extended.
        for index in [2, 6] {
            supplied[index].digest = effective_digest(&authentic[index]).unwrap().to_vec();
            supplied[index].event_type = 10;
            supplied[index].name = "opaque-firmware".into();
            supplied[index].payload.clear();
            supplied[index].version = 1;
            supplied[index].preimage = None;
        }
        assert_eq!(report(&supplied).rtmr3, quoted.rtmr3);
        assert!(verify_tdx_event_log(&quoted, &supplied, PROFILE).is_err());
    }
}

#[test]
fn opaque_imr3_relabel_cannot_hide_legacy_duplicate_identity() {
    let mut authentic = boot(1);
    authentic[5] = runtime("mr-kms", &[0x44; 32], 1);
    authentic.push(runtime("compose-hash", &[0x99; 32], 1));
    let quoted = report(&authentic);
    assert!(verify_tdx_event_log(&quoted, &authentic, TdxRuntimeProfile::LegacyMrKms).is_err());
    let mut supplied = authentic.clone();
    supplied[2].digest = effective_digest(&authentic[2]).unwrap().to_vec();
    supplied[2].event_type = 1;
    supplied[2].name = "firmware".into();
    supplied[2].payload.clear();
    assert_eq!(report(&supplied).rtmr3, quoted.rtmr3);
    assert!(verify_tdx_event_log(&quoted, &supplied, TdxRuntimeProfile::LegacyMrKms).is_err());
}

#[test]
fn firmware_stays_in_boot_registers_and_runtime_stays_in_application_register() {
    for profile in [PROFILE, TdxRuntimeProfile::LegacyMrKms] {
        let mut valid = boot(2);
        if profile == TdxRuntimeProfile::LegacyMrKms {
            valid[5] = runtime("mr-kms", &[0x44; 32], 2);
        }
        for imr in 0..3 {
            valid.push(TdxEvent {
                imr,
                event_type: 10,
                name: "firmware".into(),
                digest: vec![0x55; 48],
                payload: Vec::new(),
                version: 1,
                preimage: None,
            });
        }
        assert!(verify_tdx_event_log(&report(&valid), &valid, profile).is_ok());

        for imr in 0..3 {
            let mut invalid = valid.clone();
            let mut event = runtime("application-event", b"not a firmware event", 2);
            event.imr = imr;
            invalid.push(event);
            // Reject even if the supplied log exactly matches all quoted folds.
            assert!(verify_tdx_event_log(&report(&invalid), &invalid, profile).is_err());
        }
    }
}

#[test]
fn runtime_profile_requires_a_complete_nonconflicting_launch_tuple() {
    let events = boot(2);
    let report = report(&events);
    let identity = TdxLaunchIdentity {
        mr_td: report.mr_td,
        rtmr0: report.rtmr0,
        rtmr1: report.rtmr1,
        rtmr2: report.rtmr2,
    };
    let set = TdxMeasurementSet::with_profiles(vec![(identity.clone(), PROFILE)]);
    assert_eq!(set.runtime_profile(&report), Some(PROFILE));
    let conflicting = TdxMeasurementSet::with_profiles(vec![
        (identity.clone(), PROFILE),
        (identity.clone(), TdxRuntimeProfile::LegacyMrKms),
    ]);
    assert_eq!(conflicting.runtime_profile(&report), None);
    // None-mode has no KMS OS event: each independently pinned launch
    // component must remain mandatory, never selected from a partial match.
    for register in 0..4 {
        let mut wrong = identity.clone();
        match register {
            0 => wrong.mr_td[0] ^= 1,
            1 => wrong.rtmr0[0] ^= 1,
            2 => wrong.rtmr1[0] ^= 1,
            _ => wrong.rtmr2[0] ^= 1,
        }
        let rejected = TdxMeasurementSet::with_profiles(vec![(wrong, PROFILE)]);
        assert_eq!(rejected.runtime_profile(&report), None);
    }
}

#[test]
fn wire_parser_preserves_v2_metadata_and_rejects_unknown_or_unbounded_input() {
    let v2 = runtime("compose-hash", &[0xab, 0xcd], 2);
    let mut entry: TdxEventEntry = serde_json::from_value(serde_json::json!({
        "imr": 3, "event_type": DSTACK_RUNTIME_EVENT_TYPE, "event": v2.name,
        "digest": hex::encode(&v2.digest), "event_payload": hex::encode(&v2.payload),
        "version": 2, "preimage": hex::encode(v2.preimage.as_ref().unwrap()),
    }))
    .unwrap();
    let parsed = parse_events(&[entry.clone()]).unwrap();
    assert_eq!(
        effective_digest(&parsed[0]).unwrap(),
        effective_digest(&v2).unwrap()
    );
    entry.version = 3;
    assert!(parse_events(&[entry.clone()]).is_err());
    entry.version = 2;
    entry.preimage = Some("x".into());
    assert!(parse_events(&[entry.clone()]).is_err());
    entry.preimage = Some("00".repeat(12 * 1024 + 1));
    assert!(parse_events(&[entry]).is_err());
}

#[test]
fn legacy_wire_encoding_remains_unchanged() {
    let value = serde_json::json!({
        "imr": 3, "event_type": DSTACK_RUNTIME_EVENT_TYPE, "event": "legacy",
        "digest": "", "event_payload": "",
    });
    let entry: TdxEventEntry = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(entry.version, 1);
    assert_eq!(entry.preimage, None);
    assert_eq!(serde_json::to_value(entry).unwrap(), value);
}
