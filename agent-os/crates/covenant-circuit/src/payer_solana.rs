use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    pubkey::Pubkey,
    signer::{
        keypair::{read_keypair_file, Keypair},
        Signer as SolanaKeypairSigner,
    },
    transaction::Transaction,
};
use spl_associated_token_account::{
    get_associated_token_address_with_program_id,
    instruction::create_associated_token_account_idempotent,
};

use crate::payer::{CircPayer, TokenTransfer};
use crate::{circ, CircuitError, Result};

/// Settles a 402-quoted Token-2022 transfer on Solana and returns the confirmed signature.
///
/// Builds `[create_recipient_ata_idempotent, transfer_checked]` under the mint's token
/// program — both CIRC and $CVNT carry only metadata extensions (no transfer fee, no hook),
/// so a plain `transfer_checked` settles them exactly — then submits and confirms. The mint,
/// program, and decimals come from the quote, so the same funder pays whichever token the
/// engine selected. The funding key lives only here. Enable with the crate's `solana`
/// feature for an explicit integration; covenantd does not construct this payer.
pub struct SolanaCircPayer {
    rpc: Arc<RpcClient>,
    funder: Arc<Keypair>,
    address: String,
}

impl SolanaCircPayer {
    /// Load the funder from a Solana CLI keypair file and target `rpc_url` (defaults to
    /// Circuit's mainnet RPC).
    pub fn from_keypair_file(keypair_path: &str, rpc_url: Option<&str>) -> Result<Self> {
        let funder = read_keypair_file(keypair_path)
            .map_err(|e| CircuitError::Payment(format!("read keypair {keypair_path}: {e}")))?;
        let rpc = RpcClient::new(rpc_url.unwrap_or(circ::RPC_URL).to_string());
        Self::new(Arc::new(rpc), Arc::new(funder))
    }

    pub fn new(rpc: Arc<RpcClient>, funder: Arc<Keypair>) -> Result<Self> {
        let address = funder.pubkey().to_string();
        Ok(Self {
            rpc,
            funder,
            address,
        })
    }
}

#[async_trait]
impl CircPayer for SolanaCircPayer {
    async fn pay(&self, transfer: &TokenTransfer<'_>) -> Result<String> {
        let recipient = Pubkey::from_str(transfer.recipient).map_err(|e| {
            CircuitError::Payment(format!("bad recipient {}: {e}", transfer.recipient))
        })?;
        let mint = Pubkey::from_str(transfer.mint)
            .map_err(|e| CircuitError::Payment(format!("bad mint {}: {e}", transfer.mint)))?;
        let token_program = Pubkey::from_str(transfer.token_program).map_err(|e| {
            CircuitError::Payment(format!("bad token program {}: {e}", transfer.token_program))
        })?;
        let funder_pk = self.funder.pubkey();

        // Every ATA derivation AND the transfer must use the SAME token program id —
        // a classic-id leak points at a nonexistent account.
        let source_ata =
            get_associated_token_address_with_program_id(&funder_pk, &mint, &token_program);
        let dest_ata =
            get_associated_token_address_with_program_id(&recipient, &mint, &token_program);

        let create_dest = create_associated_token_account_idempotent(
            &funder_pk,
            &recipient,
            &mint,
            &token_program,
        );
        let transfer_ix = spl_token_2022_interface::instruction::transfer_checked(
            &token_program,
            &source_ata,
            &mint,
            &dest_ata,
            &funder_pk,
            &[&funder_pk],
            transfer.amount_raw,
            transfer.decimals,
        )
        .map_err(|e| CircuitError::Payment(format!("build transfer_checked: {e}")))?;

        let blockhash = self
            .rpc
            .get_latest_blockhash()
            .await
            .map_err(|e| CircuitError::Payment(format!("get_latest_blockhash: {e}")))?;
        let mut tx = Transaction::new_with_payer(&[create_dest, transfer_ix], Some(&funder_pk));
        tx.try_sign(&[self.funder.as_ref()], blockhash)
            .map_err(|e| CircuitError::Payment(format!("sign: {e}")))?;

        let sig = self
            .rpc
            .send_and_confirm_transaction(&tx)
            .await
            .map_err(|e| CircuitError::Payment(format!("send_and_confirm: {e}")))?;
        Ok(sig.to_string())
    }

    fn address(&self) -> Option<&str> {
        Some(&self.address)
    }
}
