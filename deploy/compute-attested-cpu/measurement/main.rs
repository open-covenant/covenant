use anyhow::{Context, ensure};
fn main() -> anyhow::Result<()> {
 let path=std::env::args().nth(1).context("vm config path required")?;
 let raw=std::fs::read(path)?;
 let raw_value: serde_json::Value=serde_json::from_slice(&raw)?;
 ensure!(raw_value["tdx_attestation_variant"]=="lite", "explicit lite profile required");
 let vm: dstack_types::VmConfig=serde_json::from_slice(&raw)?;
 let pinned="ec43cb8c5e04b6bb6c53b33b16076a01d83d01f7f4b703f2f78a0d947d6247e6";
 ensure!(hex::encode(&vm.os_image_hash)==pinned,"unapproved image hash");
 let doc=vm.tdx_measurement.as_ref().context("missing lite measurement document")?;
 doc.verify(&vm.os_image_hash).map_err(anyhow::Error::msg)?;
 let measurement=doc.decode_measurement().map_err(anyhow::Error::msg)?;
 ensure!(vm.ovmf_variant==Some(measurement.tdvf.ovmf_variant),"OVMF mismatch");
 let acpi=dstack_mr::tdx::expected_rtmr0_acpi_hashes(&vm,measurement.tdvf.ovmf_variant)?;
 let m=dstack_mr::tdx::tdx_measurements_from_measurement_document(doc,&vm,&acpi)?;
 println!("{}",serde_json::to_string_pretty(&m)?);
 Ok(())
}
