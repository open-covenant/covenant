//! Best-effort GPU detection, so a node with a GPU advertises its real
//! hardware and VRAM without the operator hand-editing `node.env`. Boot
//! probes only when neither `COVENANT_COMPUTE_NODE_HARDWARE` nor
//! `..._VRAM_GB` is set: `nvidia-smi` for an NVIDIA card, then Apple
//! Silicon's unified memory on macOS. Any absence or error reads as "no
//! GPU" and the node advertises CPU. The operator's own values always
//! win — detection fills a blank, never overrides a choice.

use std::time::Duration;

use covenant_compute_protocol::HardwareClass;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGpu {
    /// The device name `nvidia-smi` reports, e.g. `NVIDIA GeForce RTX 4090`.
    pub model: String,
    /// Total device memory rounded to the nearest GiB.
    pub vram_gb: u32,
}

impl DetectedGpu {
    /// Datacenter parts (A100/H100/L40/…) advertise `DatacenterGpu`,
    /// every other NVIDIA part `ConsumerGpu`. The class only labels the
    /// advertisement; a job's capability match keys off the model string
    /// and VRAM, so a mislabelled class never mis-routes a job.
    pub fn hardware_class(&self) -> HardwareClass {
        if is_datacenter_model(&self.model) {
            HardwareClass::DatacenterGpu {
                model: self.model.clone(),
            }
        } else {
            HardwareClass::ConsumerGpu {
                model: self.model.clone(),
            }
        }
    }

    /// The `COVENANT_COMPUTE_NODE_HARDWARE` value that advertises this
    /// GPU, exactly the string [`parse_hardware`] reads back into
    /// [`Self::hardware_class`]. `setup` pins this so what the wizard
    /// writes is what boot declares, round-trip guaranteed by the test
    /// below.
    pub fn hardware_env_value(&self) -> String {
        match self.hardware_class() {
            HardwareClass::DatacenterGpu { model } => format!("datacenter:{model}"),
            HardwareClass::ConsumerGpu { model } => format!("consumer:{model}"),
            HardwareClass::CpuOnly => "cpu".to_string(),
        }
    }
}

/// Parses a `COVENANT_COMPUTE_NODE_HARDWARE` value: `consumer:<model>`
/// or `datacenter:<model>` name a GPU, anything else is CPU-only. The
/// split takes the first colon only, so a model name carrying one
/// survives. The inverse of [`DetectedGpu::hardware_env_value`], and the
/// single parser both the boot profile and any hand-written `node.env`
/// go through.
pub fn parse_hardware(raw: &str) -> HardwareClass {
    match raw.split_once(':') {
        Some(("consumer", model)) => HardwareClass::ConsumerGpu {
            model: model.trim().to_string(),
        },
        Some(("datacenter", model)) => HardwareClass::DatacenterGpu {
            model: model.trim().to_string(),
        },
        _ => HardwareClass::CpuOnly,
    }
}

fn is_datacenter_model(model: &str) -> bool {
    let m = model.to_ascii_uppercase();
    [
        "A100", "H100", "H200", "GH200", "B100", "B200", "L40", "L4", "A40", "A30", "A10", "A16",
        "V100", "T4", "P100",
    ]
    .iter()
    .any(|tag| m.contains(tag))
}

/// Runs `nvidia-smi` and returns the first GPU it reports, or `None` on
/// any failure — the binary is absent (the common case off a GPU host),
/// the call errors, or the output does not parse. Bounded so a wedged
/// driver stalls detection, not boot.
pub async fn detect_nvidia_gpu() -> Option<DetectedGpu> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("nvidia-smi")
            .args([
                "--query-gpu=name,memory.total",
                "--format=csv,noheader,nounits",
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_nvidia_smi(&String::from_utf8_lossy(&output.stdout))
}

/// Parses the `name, memory.total` CSV `nvidia-smi` emits under
/// `--format=csv,noheader,nounits` (memory in MiB) and returns the first
/// GPU. A device name could in principle carry a comma, so the split
/// takes the last one, which fences the numeric field.
fn parse_nvidia_smi(csv: &str) -> Option<DetectedGpu> {
    let line = csv.lines().find(|l| !l.trim().is_empty())?;
    let (name, mib) = line.rsplit_once(',')?;
    let mib: u64 = mib.trim().parse().ok()?;
    let vram_gb = ((mib as f64) / 1024.0).round() as u32;
    let model = name.trim().to_string();
    if model.is_empty() || vram_gb == 0 {
        return None;
    }
    Some(DetectedGpu { model, vram_gb })
}

/// The node's hardware probe: a real NVIDIA card if `nvidia-smi` reports
/// one, else an Apple Silicon integrated GPU on macOS, else `None` (the
/// node advertises CPU). One entry point so boot and the `setup` wizard
/// agree on what a node with no hand-set hardware advertises.
pub async fn detect_gpu() -> Option<DetectedGpu> {
    if let Some(nvidia) = detect_nvidia_gpu().await {
        return Some(nvidia);
    }
    detect_apple_gpu().await
}

/// Best-effort Apple Silicon detection. An M-series Mac serves models on
/// its integrated Metal GPU out of unified memory, so a node here is a
/// real GPU node even though `nvidia-smi` reports nothing — advertising
/// CPU-only would keep it from ever matching a GPU job it can serve
/// through ollama's Metal backend, the common "non-developer plugs in
/// their laptop" case. Non-macOS, an Intel Mac, or any probe failure
/// reads as "no GPU".
#[cfg(target_os = "macos")]
pub async fn detect_apple_gpu() -> Option<DetectedGpu> {
    let brand = sysctl_string("machdep.cpu.brand_string").await?;
    let memsize_bytes: u64 = sysctl_string("hw.memsize").await?.parse().ok()?;
    apple_gpu_from(&brand, memsize_bytes)
}

#[cfg(not(target_os = "macos"))]
pub async fn detect_apple_gpu() -> Option<DetectedGpu> {
    None
}

/// Maps `machdep.cpu.brand_string` and `hw.memsize` (bytes) to the GPU an
/// Apple Silicon node advertises, or `None` for an Intel Mac or unusable
/// memory. The advertised VRAM is a deliberately conservative two-thirds
/// of unified memory (near Apple's own default Metal wired-memory limit):
/// the GPU shares that memory with the OS, and under-claiming is the safe
/// direction — the coordinator just won't match a job whose VRAM ask this
/// node can't meet, whereas over-claiming would admit a job that then
/// can't fit. Pure so the rule is tested without a host; the operator's
/// own `..._VRAM_GB`/`..._HARDWARE` override it either way.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn apple_gpu_from(brand: &str, memsize_bytes: u64) -> Option<DetectedGpu> {
    // Apple Silicon reports "Apple M<n> …"; an Intel Mac reports its
    // Intel CPU string, which has no integrated GPU worth advertising.
    if !brand.starts_with("Apple ") {
        return None;
    }
    let total_gb = memsize_bytes / 1_073_741_824;
    let vram_gb = u32::try_from(total_gb * 2 / 3).ok()?;
    if vram_gb == 0 {
        return None;
    }
    Some(DetectedGpu {
        model: brand.to_string(),
        vram_gb,
    })
}

/// Runs `sysctl -n <key>` and returns its trimmed stdout, or `None` on a
/// missing binary, an error, a timeout, or empty output. Bounded so a
/// wedged call stalls detection, not boot — the same posture as the
/// `nvidia-smi` probe.
#[cfg(target_os = "macos")]
async fn sysctl_string(key: &str) -> Option<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("sysctl").args(["-n", key]).output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_consumer_card() {
        let gpu = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 24564\n").unwrap();
        assert_eq!(gpu.model, "NVIDIA GeForce RTX 4090");
        assert_eq!(gpu.vram_gb, 24);
        assert_eq!(
            gpu.hardware_class(),
            HardwareClass::ConsumerGpu {
                model: "NVIDIA GeForce RTX 4090".into()
            }
        );
    }

    #[test]
    fn parses_a_datacenter_card_and_labels_it() {
        let gpu = parse_nvidia_smi("NVIDIA H100 80GB HBM3, 81559").unwrap();
        assert_eq!(gpu.vram_gb, 80);
        assert_eq!(
            gpu.hardware_class(),
            HardwareClass::DatacenterGpu {
                model: "NVIDIA H100 80GB HBM3".into()
            }
        );
    }

    #[test]
    fn takes_the_first_of_several_gpus() {
        let gpu = parse_nvidia_smi("NVIDIA A100-SXM4-40GB, 40960\nNVIDIA A100-SXM4-40GB, 40960\n")
            .unwrap();
        assert_eq!(gpu.vram_gb, 40);
        assert!(matches!(
            gpu.hardware_class(),
            HardwareClass::DatacenterGpu { .. }
        ));
    }

    #[test]
    fn rejects_junk_and_zero_memory() {
        assert!(parse_nvidia_smi("").is_none());
        assert!(parse_nvidia_smi("no comma here").is_none());
        assert!(parse_nvidia_smi("NVIDIA Fake, notanumber").is_none());
        assert!(parse_nvidia_smi("NVIDIA Fake, 0").is_none());
        assert!(parse_nvidia_smi(", 24564").is_none());
    }

    #[test]
    fn hardware_env_value_labels_consumer_and_datacenter() {
        assert_eq!(
            DetectedGpu {
                model: "NVIDIA GeForce RTX 4090".into(),
                vram_gb: 24
            }
            .hardware_env_value(),
            "consumer:NVIDIA GeForce RTX 4090"
        );
        assert_eq!(
            DetectedGpu {
                model: "NVIDIA H100 80GB HBM3".into(),
                vram_gb: 80
            }
            .hardware_env_value(),
            "datacenter:NVIDIA H100 80GB HBM3"
        );
    }

    #[test]
    fn parse_hardware_reads_gpu_classes_and_falls_back_to_cpu() {
        assert_eq!(
            parse_hardware("consumer:rtx4090"),
            HardwareClass::ConsumerGpu {
                model: "rtx4090".into()
            }
        );
        assert_eq!(
            parse_hardware("datacenter: A100 "),
            HardwareClass::DatacenterGpu {
                model: "A100".into()
            }
        );
        assert_eq!(parse_hardware("cpu"), HardwareClass::CpuOnly);
        assert_eq!(parse_hardware("nonsense"), HardwareClass::CpuOnly);
    }

    #[test]
    fn hardware_env_value_round_trips_through_parse_hardware() {
        // What `setup` pins must parse back at boot to the same class,
        // including model names with spaces, hyphens, or a stray colon.
        for model in [
            "NVIDIA GeForce RTX 4090",
            "NVIDIA H100 80GB HBM3",
            "NVIDIA A100-SXM4-40GB",
            "NVIDIA RTX: Odd Name",
        ] {
            let gpu = DetectedGpu {
                model: model.into(),
                vram_gb: 24,
            };
            let value = gpu.hardware_env_value();
            assert_eq!(
                parse_hardware(&value),
                gpu.hardware_class(),
                "setup pins {value:?}; boot must parse it back to the same class"
            );
        }
    }

    #[tokio::test]
    async fn detection_returns_promptly_and_sanely() {
        // Off a GPU host (CI, most dev machines) this is None; on a GPU
        // host it is Some with a nonzero VRAM. Either way it must return
        // within the bound and never panic.
        if let Some(gpu) = detect_nvidia_gpu().await {
            assert!(gpu.vram_gb > 0, "a detected GPU reports nonzero VRAM");
            assert!(!gpu.model.is_empty());
        }
    }

    const GIB: u64 = 1_073_741_824;

    #[test]
    fn apple_gpu_advertises_two_thirds_of_unified_memory_as_consumer() {
        let gpu = apple_gpu_from("Apple M4 Pro", 24 * GIB).unwrap();
        assert_eq!(gpu.model, "Apple M4 Pro");
        assert_eq!(gpu.vram_gb, 16); // 24 * 2 / 3
        assert_eq!(
            gpu.hardware_class(),
            HardwareClass::ConsumerGpu {
                model: "Apple M4 Pro".into()
            }
        );
        assert_eq!(
            apple_gpu_from("Apple M2 Max", 96 * GIB).unwrap().vram_gb,
            64
        );
        assert_eq!(apple_gpu_from("Apple M1", 8 * GIB).unwrap().vram_gb, 5); // floors
    }

    #[test]
    fn apple_gpu_rejects_intel_macs_and_unusable_memory() {
        // An Intel Mac reports an Intel brand string, no integrated GPU
        // worth advertising.
        assert!(apple_gpu_from("Intel(R) Core(TM) i9-9980HK CPU @ 2.40GHz", 32 * GIB).is_none());
        // Under 2 GiB the conservative fraction floors to zero VRAM,
        // which is no better than advertising CPU.
        assert!(apple_gpu_from("Apple M1", GIB).is_none());
        assert!(apple_gpu_from("Apple M1", 0).is_none());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn apple_detection_returns_promptly_and_sanely() {
        // On Apple Silicon this is Some with a nonzero VRAM and an
        // "Apple …" model; on an Intel Mac it is None. Either way it must
        // return within the bound and never panic.
        if let Some(gpu) = detect_apple_gpu().await {
            assert!(gpu.model.starts_with("Apple "), "got: {}", gpu.model);
            assert!(gpu.vram_gb > 0);
        }
    }
}
