use covenant_inference_node::tunnel::signed_registration;
use ed25519_dalek::SigningKey;

fn key() -> SigningKey {
    let mut seed = [0_u8; 32];
    getrandom::getrandom(&mut seed).unwrap();
    SigningKey::from_bytes(&seed)
}

#[test]
fn registration_signs_and_verifies_against_the_node_key() {
    let key = key();
    let registration = signed_registration("slot-1", &key).unwrap();
    assert!(registration.verify(&key.verifying_key()).is_ok());
}

#[test]
fn registration_with_a_swapped_node_id_is_rejected() {
    let key = key();
    let mut registration = signed_registration("slot-1", &key).unwrap();
    registration.node_id = "0xdeadbeef".to_owned();
    assert!(registration.verify(&key.verifying_key()).is_err());
}

#[test]
fn registration_with_a_shifted_issued_at_is_rejected() {
    let key = key();
    let mut registration = signed_registration("slot-1", &key).unwrap();
    // issued_at is inside the signed payload, so the gateway's freshness check
    // can't be forged by back-dating a captured registration.
    registration.issued_at -= chrono::Duration::seconds(90);
    assert!(registration.verify(&key.verifying_key()).is_err());
}
