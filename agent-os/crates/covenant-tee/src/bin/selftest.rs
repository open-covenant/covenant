use covenant_tee::{attest, signer_from_keypair_file, verify_attestation};
use sha2::{Digest, Sha256};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let tee = std::env::args().nth(1).unwrap_or_else(|| "https://mainnet-tee.magicblock.app".into());
    let key = std::env::var("COVENANT_ATTESTER_KEY")
        .unwrap_or_else(|_| format!("{}/.config/solana/covenant-agent.json", std::env::var("HOME").unwrap()));
    let (sk, pubkey) = signer_from_keypair_file(&key)?;
    println!("attester : {pubkey}");
    println!("tee      : {tee}");

    let provenance = hex::encode(Sha256::digest(b"sample agent action chain"));
    let att = attest(&tee, &sk, &pubkey, Some(&pubkey), Some(&provenance), None).await?;
    let v = &att.body.er.validator.clone().unwrap_or_default();
    println!("validator: {v}");
    println!("enclave  : TCB {} | mr_td {}...", att.body.enclave.status, &att.body.enclave.mr_td[..18]);

    verify_attestation(&att, &[pubkey.clone()])?;
    println!("verify (trusted)  : ok");
    match verify_attestation(&att, &["11111111111111111111111111111111".into()]) {
        Err(e) => println!("verify (untrusted): rejected ({e})"),
        Ok(()) => anyhow::bail!("untrusted key was wrongly accepted"),
    }
    println!("\nCOVENANT TEE ATTESTATION (Rust, agent-bound) VERIFIED");
    Ok(())
}
