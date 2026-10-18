#[cfg(kani)]
#[kani::proof]
fn receipt_smoke() {
    let value: u8 = kani::any();
    kani::assume(value < 10);
    kani::cover!(value == 4, "smoke reachable");
    assert!(value < 10);
}

#[cfg(kani)]
#[kani::proof]
fn receipt_refute() {
    let value: u8 = kani::any();
    kani::assume(value < 10);
    assert!(value == 0, "COVENANT_SMOKE_EXPECTED_ASSERTION");
}
