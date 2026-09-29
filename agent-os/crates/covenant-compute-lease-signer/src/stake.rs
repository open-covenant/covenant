//! An operator's stake in the settlement program.
//!
//! The node's identity is the position's agent, the tokens sit in a vault the
//! program owns, and only the slash authority can take them before the lock
//! ends. The coordinator counts a position toward an operator while it stays
//! locked past a lease's window and the dispute window.

use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use spl_associated_token_account::get_associated_token_address_with_program_id;

use crate::lease::{discriminator, SYSTEM_PROGRAM};

pub const DEFAULT_PROGRAM: Pubkey =
    Pubkey::from_str_const("3dTtXH7rah8YyWTsAfSg6qC3iqrJXWGEhAx57uUHXZff");

/// `StakePosition`'s size on chain, discriminator included.
pub const POSITION_LEN: usize = 122;

/// Every account a stake from `owner` for the node `agent_key` touches.
#[derive(Debug, Clone, Copy)]
pub struct StakeAccounts {
    pub program: Pubkey,
    pub agent_key: [u8; 32],
    pub owner: Pubkey,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub config: Pubkey,
    pub agent: Pubkey,
    pub position: Pubkey,
    /// The position's own token account: the program signs for it, nobody
    /// else can.
    pub vault: Pubkey,
    pub owner_tokens: Pubkey,
}

impl StakeAccounts {
    pub fn derive(
        program: Pubkey,
        agent_key: [u8; 32],
        owner: Pubkey,
        mint: Pubkey,
        token_program: Pubkey,
    ) -> Self {
        let config = Pubkey::find_program_address(&[b"config"], &program).0;
        let agent = Pubkey::find_program_address(&[b"agent", &agent_key], &program).0;
        let position =
            Pubkey::find_program_address(&[b"stake", &agent_key, owner.as_ref()], &program).0;
        Self {
            program,
            agent_key,
            owner,
            mint,
            token_program,
            config,
            agent,
            position,
            vault: get_associated_token_address_with_program_id(&position, &mint, &token_program),
            owner_tokens: get_associated_token_address_with_program_id(
                &owner,
                &mint,
                &token_program,
            ),
        }
    }

    /// Registers the node as an agent, owned by `owner`. A stake needs one.
    pub fn register_agent(&self) -> Instruction {
        let mut data = discriminator("global", "register_agent").to_vec();
        data.extend_from_slice(&self.agent_key);
        data.extend_from_slice(&[0u8; 32]);
        data.extend_from_slice(&[0u8; 32]);
        Instruction {
            program_id: self.program,
            accounts: vec![
                AccountMeta::new_readonly(self.config, false),
                AccountMeta::new(self.agent, false),
                AccountMeta::new(self.owner, true),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            data,
        }
    }

    pub fn stake(&self, amount: u64, lock_until: u64) -> Instruction {
        let mut data = discriminator("global", "stake").to_vec();
        data.extend_from_slice(&amount.to_le_bytes());
        data.extend_from_slice(&lock_until.to_le_bytes());
        Instruction {
            program_id: self.program,
            accounts: vec![
                AccountMeta::new_readonly(self.config, false),
                AccountMeta::new(self.agent, false),
                AccountMeta::new(self.position, false),
                AccountMeta::new(self.owner, true),
                AccountMeta::new(self.owner_tokens, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new_readonly(self.token_program, false),
                AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
            ],
            data,
        }
    }

    pub fn unstake(&self) -> Instruction {
        Instruction {
            program_id: self.program,
            accounts: vec![
                AccountMeta::new_readonly(self.config, false),
                AccountMeta::new(self.agent, false),
                AccountMeta::new(self.position, false),
                AccountMeta::new(self.owner, true),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(self.owner_tokens, false),
                AccountMeta::new_readonly(self.mint, false),
                AccountMeta::new_readonly(self.token_program, false),
            ],
            data: discriminator("global", "unstake").to_vec(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Position {
    pub owner: Pubkey,
    pub amount: u64,
    pub lock_until: u64,
    pub active: bool,
}

impl Position {
    pub fn decode(data: &[u8]) -> Option<Self> {
        if data.len() != POSITION_LEN || data[..8] != discriminator("account", "StakePosition") {
            return None;
        }
        let u64_at = |offset: usize| {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&data[offset..offset + 8]);
            u64::from_le_bytes(bytes)
        };
        let mut owner = [0u8; 32];
        owner.copy_from_slice(&data[40..72]);
        Some(Self {
            owner: Pubkey::new_from_array(owner),
            amount: u64_at(72),
            lock_until: u64_at(80),
            active: data[120] == 1,
        })
    }
}

/// The stake mint the protocol config names, and its minimum lock.
pub fn config_mint_and_min_lock(config_data: &[u8]) -> Option<(Pubkey, u64)> {
    if config_data.len() < 154 || config_data[..8] != discriminator("account", "Config") {
        return None;
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&config_data[72..104]);
    let mut lock = [0u8; 8];
    lock.copy_from_slice(&config_data[146..154]);
    Some((Pubkey::new_from_array(mint), u64::from_le_bytes(lock)))
}

/// An SPL mint's decimals, the same byte under both token programs.
pub fn mint_decimals(mint_data: &[u8]) -> Option<u8> {
    mint_data.get(44).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    #[test]
    fn the_stake_lands_in_a_vault_only_the_position_controls() {
        let accounts = StakeAccounts::derive(DEFAULT_PROGRAM, [7u8; 32], key(1), key(2), key(3));
        let expected = Pubkey::find_program_address(
            &[b"stake", &[7u8; 32], key(1).as_ref()],
            &DEFAULT_PROGRAM,
        )
        .0;
        assert_eq!(accounts.position, expected);
        assert_eq!(
            accounts.vault,
            get_associated_token_address_with_program_id(&expected, &key(2), &key(3))
        );
        let ix = accounts.stake(5, 9);
        let keys: Vec<Pubkey> = ix.accounts.iter().map(|a| a.pubkey).collect();
        assert_eq!(
            keys,
            vec![
                accounts.config,
                accounts.agent,
                accounts.position,
                key(1),
                accounts.owner_tokens,
                accounts.vault,
                key(2),
                key(3),
                SYSTEM_PROGRAM
            ]
        );
        assert!(ix.accounts[3].is_signer);
        assert_eq!(&ix.data[8..16], &5u64.to_le_bytes());
        assert_eq!(&ix.data[16..24], &9u64.to_le_bytes());
    }

    #[test]
    fn a_position_decodes_at_the_program_offsets() {
        let mut data = discriminator("account", "StakePosition").to_vec();
        data.extend_from_slice(&[7u8; 32]);
        data.extend_from_slice(key(1).as_ref());
        data.extend_from_slice(&1_000_000_000_000u64.to_le_bytes());
        data.extend_from_slice(&1_900_000_000u64.to_le_bytes());
        data.extend_from_slice(key(4).as_ref());
        data.extend_from_slice(&[1, 250]);
        let position = Position::decode(&data).unwrap();
        assert_eq!(position.owner, key(1));
        assert_eq!(position.amount, 1_000_000_000_000);
        assert_eq!(position.lock_until, 1_900_000_000);
        assert!(position.active);
    }
}
