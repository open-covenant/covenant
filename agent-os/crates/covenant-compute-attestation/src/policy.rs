use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::vendor_types::SnpTcb;
use chrono::Duration;
use p384::elliptic_curve::subtle::{Choice, ConstantTimeEq};
use serde::Deserialize;

use crate::snp::ProductLine;
use crate::tdx::{TdxLaunchIdentity, TdxMeasurementSet, TdxRuntimeProfile};

/// Reference measurements are compiled in rather than fetched, so a verdict
/// cannot be widened by anything an operator can reach at runtime.
const MEASUREMENT_ALLOWLIST: &str = include_str!("../reference/h100-measurements.json");
const SNP_PLATFORM: &str = include_str!("../reference/snp-platform.json");
const SNP_LAUNCH_MEASUREMENTS: &str = include_str!("../reference/snp-launch-measurements.json");
const TDX_LAUNCH_MEASUREMENTS: &str = include_str!("../reference/tdx-launch-measurements.json");

const ALLOWLIST_SCHEMA_VERSION: u32 = 2;
// Separate files, separate schemas. Sharing one constant meant widening the
// measurement format silently invalidated the others.
const PLATFORM_SCHEMA_VERSION: u32 = 1;
const LAUNCH_SCHEMA_VERSION: u32 = 1;
const TDX_LAUNCH_SCHEMA_VERSION: u32 = 1;

const DEFAULT_VERDICT_TTL_HOURS: i64 = 24;

/// A node verdict expires so the node has to answer a fresh challenge. A lease
/// verdict is about one VM on one lease and cannot be reused anywhere else, so
/// it lives with the lease instead. The control plane sets this from the
/// lease's end; the default is only a ceiling for a caller that does not.
const DEFAULT_LEASE_VERDICT_TTL_DAYS: i64 = 7;

/// Knobs the control plane may set. Everything that could weaken a verdict is
/// unreachable from a service build; there is deliberately no environment
/// variable that relaxes verification.
#[derive(Debug, Clone)]
pub struct Policy {
    /// How long a verdict stays good before the node has to answer a fresh
    /// challenge.
    pub verdict_ttl: Duration,
    /// How long a guest verdict stays good. Set from the lease it belongs to.
    pub lease_verdict_ttl: Duration,
    allow_expired_certificates: bool,
    /// Not even present in a service build.
    #[cfg(any(test, feature = "test-vectors"))]
    trust_test_root: bool,
    /// Launch identities the tests accept in place of the compiled reference
    /// set. A genuine Intel-signed quote cannot be fabricated the way the
    /// NVIDIA and SNP report builders fabricate reports, so the vectors verify
    /// a real vendored quote and inject its identity here. Not even present in
    /// a service build.
    #[cfg(any(test, feature = "test-vectors"))]
    tdx_test_identities: Option<TdxMeasurementSet>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            verdict_ttl: Duration::hours(DEFAULT_VERDICT_TTL_HOURS),
            lease_verdict_ttl: Duration::days(DEFAULT_LEASE_VERDICT_TTL_DAYS),
            allow_expired_certificates: false,
            #[cfg(any(test, feature = "test-vectors"))]
            trust_test_root: false,
            #[cfg(any(test, feature = "test-vectors"))]
            tdx_test_identities: None,
        }
    }
}

impl Policy {
    pub fn with_verdict_ttl(mut self, verdict_ttl: Duration) -> Self {
        self.verdict_ttl = verdict_ttl;
        self
    }

    pub fn with_lease_verdict_ttl(mut self, lease_verdict_ttl: Duration) -> Self {
        self.lease_verdict_ttl = lease_verdict_ttl;
        self
    }

    pub(crate) fn allows_expired_certificates(&self) -> bool {
        self.allow_expired_certificates
    }

    #[cfg(any(test, feature = "test-vectors"))]
    pub(crate) fn trusts_test_root(&self) -> bool {
        self.trust_test_root
    }

    /// Anchors at the locally generated root under tests/fixtures instead of
    /// the vendor root. Certificate validity is still enforced. A service
    /// builds its policy with `default`, so even a build that somehow turned
    /// the feature on keeps the vendor root as its only anchor.
    #[cfg(any(test, feature = "test-vectors"))]
    pub fn for_tests() -> Self {
        Self {
            trust_test_root: true,
            ..Self::default()
        }
    }

    /// For vectors whose certificates are deliberately out of date.
    #[cfg(any(test, feature = "test-vectors"))]
    pub fn allowing_expired_certificates(mut self) -> Self {
        self.allow_expired_certificates = true;
        self
    }

    /// Accepts these launch identities in place of the compiled TDX reference
    /// set. Vectors verify a real vendored quote, so the identity they inject
    /// is one a genuine platform actually produced.
    #[cfg(any(test, feature = "test-vectors"))]
    pub fn with_tdx_test_identities(mut self, identities: Vec<TdxLaunchIdentity>) -> Self {
        self.tdx_test_identities = Some(TdxMeasurementSet::new(identities));
        self
    }

    pub(crate) fn tdx_measurements(&self) -> &TdxMeasurementSet {
        #[cfg(any(test, feature = "test-vectors"))]
        if let Some(identities) = &self.tdx_test_identities {
            return identities;
        }
        tdx_launch_measurements()
    }
}

#[derive(Debug, Deserialize)]
struct AllowlistFile {
    schema_version: u32,
    measurements: Vec<AllowlistEntry>,
}

#[derive(Debug, Deserialize)]
struct AllowlistEntry {
    /// The slot the report identifies this measurement by. The name beside it
    /// in the reference file is a label for humans and is not compared.
    index: u32,
    name: String,
    /// A slot can have more than one legitimate value. NVIDIA's reference
    /// manifest lists alternatives for several of them, and collapsing those to
    /// one digest would reject cards that are behaving correctly.
    sha384: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct MeasurementAllowlist {
    by_index: BTreeMap<u32, Vec<[u8; 48]>>,
}

impl MeasurementAllowlist {
    /// Whether this digest is one of the values the slot may hold. Comparison
    /// stays constant time per candidate, and every candidate is compared so
    /// the answer does not leak which alternative matched through timing.
    pub(crate) fn accepts(&self, index: u32, digest: &[u8; 48]) -> Option<bool> {
        let candidates = self.by_index.get(&index)?;
        let mut hit = Choice::from(0u8);
        for candidate in candidates {
            hit |= candidate.ct_eq(digest);
        }
        Some(bool::from(hit))
    }

    pub(crate) fn len(&self) -> usize {
        self.by_index.len()
    }
}

/// Parsed once. A malformed file is a build-time mistake in this repository,
/// not something a node can trigger, so it panics rather than degrading into a
/// verifier that trusts an empty allowlist.
pub(crate) fn measurement_allowlist() -> &'static MeasurementAllowlist {
    static ALLOWLIST: OnceLock<MeasurementAllowlist> = OnceLock::new();
    ALLOWLIST.get_or_init(|| {
        let file: AllowlistFile = serde_json::from_str(MEASUREMENT_ALLOWLIST)
            .expect("reference/h100-measurements.json is not valid JSON");
        assert_eq!(
            file.schema_version, ALLOWLIST_SCHEMA_VERSION,
            "reference/h100-measurements.json schema version is not understood by this verifier"
        );
        assert!(
            !file.measurements.is_empty(),
            "reference/h100-measurements.json carries no measurements"
        );

        let mut by_index: BTreeMap<u32, Vec<[u8; 48]>> = BTreeMap::new();
        for entry in file.measurements {
            assert!(
                !entry.sha384.is_empty(),
                "measurement {} lists no digest, which would accept nothing",
                entry.name
            );
            let mut digests = Vec::with_capacity(entry.sha384.len());
            for hexed in &entry.sha384 {
                let raw = hex::decode(hexed).unwrap_or_else(|_| {
                    panic!("measurement {} has a non-hex digest", entry.name);
                });
                digests.push(raw.try_into().unwrap_or_else(|_: Vec<u8>| {
                    panic!("measurement {} is not a 48 byte SHA-384 digest", entry.name);
                }));
            }
            if by_index.insert(entry.index, digests).is_some() {
                panic!("measurement slot {} is listed twice", entry.index);
            }
        }
        MeasurementAllowlist { by_index }
    })
}

#[derive(Debug, Deserialize)]
struct PlatformFile {
    schema_version: u32,
    product_line: String,
    /// The digest of the root this build compiles in. A root file swapped
    /// without the reference being updated is a build mistake, and this is what
    /// catches it.
    ark_sha256: String,
    required_vmpl: u32,
    guest_policy: GuestPolicyFile,
    tcb_floor: TcbFile,
}

#[derive(Debug, Deserialize)]
struct GuestPolicyFile {
    debug: bool,
    migrate_ma: bool,
    smt: bool,
    single_socket: bool,
}

#[derive(Debug, Deserialize)]
struct TcbFile {
    bootloader: u8,
    tee: u8,
    snp: u8,
    microcode: u8,
}

/// The launch policy the reference guest is started under. It is a host input
/// fixed at SNP_LAUNCH and immutable afterwards, so requiring the exact word
/// says which machine shape the published measurement belongs to rather than
/// merely that debug happened to be off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RequiredGuestPolicy {
    pub(crate) debug: bool,
    pub(crate) migrate_ma: bool,
    pub(crate) smt: bool,
    pub(crate) single_socket: bool,
}

#[derive(Debug)]
pub(crate) struct SnpPlatform {
    pub(crate) product_line: ProductLine,
    pub(crate) required_vmpl: u32,
    pub(crate) guest_policy: RequiredGuestPolicy,
    pub(crate) tcb_floor: SnpTcb,
}

pub(crate) fn snp_platform() -> &'static SnpPlatform {
    static PLATFORM: OnceLock<SnpPlatform> = OnceLock::new();
    PLATFORM.get_or_init(|| {
        let file: PlatformFile = serde_json::from_str(SNP_PLATFORM)
            .expect("reference/snp-platform.json is not valid JSON");
        assert_eq!(
            file.schema_version, PLATFORM_SCHEMA_VERSION,
            "reference/snp-platform.json schema version is not understood by this verifier"
        );

        let product_line = ProductLine::parse(&file.product_line).unwrap_or_else(|| {
            panic!(
                "reference/snp-platform.json names product line {}, which has no TCB encoding here",
                file.product_line
            )
        });
        let ark_sha256: [u8; 32] = hex::decode(&file.ark_sha256)
            .ok()
            .and_then(|raw| raw.try_into().ok())
            .expect("reference/snp-platform.json ark_sha256 is not a 32 byte hex digest");
        assert_eq!(
            ark_sha256,
            crate::amd::pinned_ark_sha256(),
            "roots/amd-ark-genoa.der is not the root reference/snp-platform.json records"
        );

        SnpPlatform {
            product_line,
            required_vmpl: file.required_vmpl,
            guest_policy: RequiredGuestPolicy {
                debug: file.guest_policy.debug,
                migrate_ma: file.guest_policy.migrate_ma,
                smt: file.guest_policy.smt,
                single_socket: file.guest_policy.single_socket,
            },
            tcb_floor: SnpTcb {
                bootloader: file.tcb_floor.bootloader,
                tee: file.tcb_floor.tee,
                snp: file.tcb_floor.snp,
                microcode: file.tcb_floor.microcode,
            },
        }
    })
}

#[derive(Debug, Deserialize)]
struct LaunchMeasurementFile {
    schema_version: u32,
    measurements: Vec<LaunchMeasurementEntry>,
}

#[derive(Debug, Deserialize)]
struct LaunchMeasurementEntry {
    image_version: String,
    vcpu_count: u32,
    sha384: String,
}

/// The verifier only needs the set of digests. Which image version and machine
/// shape each one belongs to, and the inputs it was computed from, are recorded
/// in the file for the people who reproduce it.
#[derive(Debug)]
pub(crate) struct SnpLaunchMeasurements {
    digests: Vec<[u8; 48]>,
}

impl SnpLaunchMeasurements {
    /// Every entry is compared and the loop does not stop early, so the reply
    /// carries no timing signal about which shape a report claimed.
    pub(crate) fn contains(&self, measurement: &[u8; 48]) -> bool {
        let mut found = 0_u8;
        for digest in &self.digests {
            found |= digest.ct_eq(measurement).unwrap_u8();
        }
        found == 1
    }
}

pub(crate) fn snp_launch_measurements() -> &'static SnpLaunchMeasurements {
    static MEASUREMENTS: OnceLock<SnpLaunchMeasurements> = OnceLock::new();
    MEASUREMENTS.get_or_init(|| {
        let file: LaunchMeasurementFile = serde_json::from_str(SNP_LAUNCH_MEASUREMENTS)
            .expect("reference/snp-launch-measurements.json is not valid JSON");
        assert_eq!(
            file.schema_version, LAUNCH_SCHEMA_VERSION,
            "reference/snp-launch-measurements.json schema version is not understood by this verifier"
        );
        assert!(
            !file.measurements.is_empty(),
            "reference/snp-launch-measurements.json carries no measurements"
        );

        let mut digests: Vec<[u8; 48]> = Vec::with_capacity(file.measurements.len());
        for entry in file.measurements {
            let digest: [u8; 48] = hex::decode(&entry.sha384)
                .ok()
                .and_then(|raw| raw.try_into().ok())
                .unwrap_or_else(|| {
                    panic!(
                        "launch measurement {} on {} vCPUs is not a 48 byte hex digest",
                        entry.image_version, entry.vcpu_count
                    )
                });
            if digests.contains(&digest) {
                panic!(
                    "launch measurement {} on {} vCPUs is listed twice",
                    entry.image_version, entry.vcpu_count
                );
            }
            digests.push(digest);
        }
        SnpLaunchMeasurements { digests }
    })
}

#[derive(Debug, Deserialize)]
struct TdxLaunchFile {
    schema_version: u32,
    launch_measurements: Vec<TdxLaunchEntry>,
}

#[derive(Debug, Deserialize)]
struct TdxLaunchEntry {
    image_version: String,
    mr_td: String,
    rtmr0: String,
    rtmr1: String,
    rtmr2: String,
    #[serde(default)]
    runtime_profile: TdxRuntimeProfileName,
    os_image_hash: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
enum TdxRuntimeProfileName {
    #[default]
    #[serde(rename = "legacy_mr_kms")]
    LegacyMrKms,
    #[serde(rename = "dstack_0_6_1")]
    Dstack061,
}

/// Unlike the SNP loader this one accepts an empty file: an empty set refuses
/// every quote rather than trusting anything, which is the correct meaning of
/// having no reference. The set is not empty today; the leeway stays because
/// emptying it deliberately (pulling a compromised image's identity, say)
/// must never turn into a build that trusts everything.
pub(crate) fn tdx_launch_measurements() -> &'static TdxMeasurementSet {
    static MEASUREMENTS: OnceLock<TdxMeasurementSet> = OnceLock::new();
    MEASUREMENTS.get_or_init(|| {
        let file: TdxLaunchFile = serde_json::from_str(TDX_LAUNCH_MEASUREMENTS)
            .expect("reference/tdx-launch-measurements.json is not valid JSON");
        assert_eq!(
            file.schema_version, TDX_LAUNCH_SCHEMA_VERSION,
            "reference/tdx-launch-measurements.json schema version is not understood by this verifier"
        );

        let digest = |label: &str, image: &str, hexed: &str| -> [u8; 48] {
            hex::decode(hexed)
                .ok()
                .and_then(|raw| raw.try_into().ok())
                .unwrap_or_else(|| {
                    panic!("launch identity {image} carries a {label} that is not a 48 byte hex digest")
                })
        };
        let identities = file
            .launch_measurements
            .iter()
            .map(|entry| {
                let profile = match entry.runtime_profile {
                    TdxRuntimeProfileName::LegacyMrKms => TdxRuntimeProfile::LegacyMrKms,
                    TdxRuntimeProfileName::Dstack061 => TdxRuntimeProfile::Dstack061 {
                        os_image_hash: entry.os_image_hash.as_ref()
                            .and_then(|s| hex::decode(s).ok())
                            .and_then(|bytes| bytes.try_into().ok())
                            .expect("dstack 0.6.1 launch identity requires a 32-byte OS image hash"),
                    },
                };
                (TdxLaunchIdentity {
                    mr_td: digest("mr_td", &entry.image_version, &entry.mr_td),
                    rtmr0: digest("rtmr0", &entry.image_version, &entry.rtmr0),
                    rtmr1: digest("rtmr1", &entry.image_version, &entry.rtmr1),
                    rtmr2: digest("rtmr2", &entry.image_version, &entry.rtmr2),
                }, profile)
            })
            .collect();
        TdxMeasurementSet::with_profiles(identities)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_parses_and_is_not_empty() {
        assert!(measurement_allowlist().len() > 0);
    }

    /// The reference set carries the identities recorded in provenance.json,
    /// reproduced from published images. A set that parses to nothing means
    /// the file and the provenance record have drifted apart.
    #[test]
    fn the_tdx_reference_parses_and_is_not_empty() {
        assert!(!tdx_launch_measurements().is_empty());
    }

    #[test]
    fn production_tdx_reference_uses_its_pinned_os_runtime_profile() {
        let file: TdxLaunchFile = serde_json::from_str(TDX_LAUNCH_MEASUREMENTS).unwrap();
        let production: Vec<_> = file
            .launch_measurements
            .iter()
            .filter(|entry| matches!(entry.runtime_profile, TdxRuntimeProfileName::Dstack061))
            .collect();
        assert!(!production.is_empty());
        for entry in production {
            let decode =
                |hexed: &str| -> [u8; 48] { hex::decode(hexed).unwrap().try_into().unwrap() };
            let mut report = crate::tdx::TdReport {
                mr_td: decode(&entry.mr_td),
                rtmr0: decode(&entry.rtmr0),
                rtmr1: decode(&entry.rtmr1),
                rtmr2: decode(&entry.rtmr2),
                rtmr3: [0; 48],
                report_data: [0; 64],
            };
            let os_image_hash = hex::decode(entry.os_image_hash.as_ref().unwrap())
                .unwrap()
                .try_into()
                .unwrap();
            assert_eq!(
                tdx_launch_measurements().runtime_profile(&report),
                Some(TdxRuntimeProfile::Dstack061 { os_image_hash })
            );
            report.rtmr2[0] ^= 1;
            assert_eq!(tdx_launch_measurements().runtime_profile(&report), None);
        }
    }

    #[test]
    fn the_default_policy_relaxes_nothing() {
        let policy = Policy::default();
        assert!(!policy.allows_expired_certificates());
        assert!(!policy.trusts_test_root());
        assert_eq!(
            policy.verdict_ttl,
            Duration::hours(DEFAULT_VERDICT_TTL_HOURS)
        );
        assert_eq!(
            policy.lease_verdict_ttl,
            Duration::days(DEFAULT_LEASE_VERDICT_TTL_DAYS)
        );
    }

    #[test]
    fn the_snp_reference_parses_and_agrees_with_the_compiled_root() {
        assert_eq!(snp_platform().product_line, ProductLine::Genoa);

        let file: LaunchMeasurementFile = serde_json::from_str(SNP_LAUNCH_MEASUREMENTS).unwrap();
        let listed: [u8; 48] = hex::decode(&file.measurements[0].sha384)
            .unwrap()
            .try_into()
            .unwrap();
        assert!(snp_launch_measurements().contains(&listed));
    }

    #[test]
    fn the_test_policy_still_enforces_validity_until_asked_not_to() {
        assert!(!Policy::for_tests().allows_expired_certificates());
        assert!(
            Policy::for_tests()
                .allowing_expired_certificates()
                .allows_expired_certificates()
        );
    }
}
