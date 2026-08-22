use solana_program::{program_error::ProgramError, pubkey::Pubkey};

use crate::error::EscrowError;

pub const FUND_DISCRIMINATOR: u8 = 0;
pub const BIND_DISCRIMINATOR: u8 = 1;
pub const RELEASE_DISCRIMINATOR: u8 = 2;
pub const REFUND_DISCRIMINATOR: u8 = 3;

pub const FUND_DATA_LEN: usize = 84;
pub const BIND_DATA_LEN: usize = 105;
pub const RESOLVE_DATA_LEN: usize = 65;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundArgs {
    pub bounty_id: [u8; 32],
    pub amount_lamports: u64,
    pub offer_expires_at: i64,
    pub acceptance_commitment: [u8; 32],
    pub state_bump: u8,
    pub vault_bump: u8,
    pub guard_bump: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindArgs {
    pub bounty_id: [u8; 32],
    pub claimant: Pubkey,
    pub claim_expires_at: i64,
    pub claim_commitment: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolveArgs {
    pub bounty_id: [u8; 32],
    pub resolution_evidence: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowInstruction {
    Fund(FundArgs),
    Bind(BindArgs),
    Release(ResolveArgs),
    Refund(ResolveArgs),
}

impl EscrowInstruction {
    pub fn decode(data: &[u8]) -> Result<Self, ProgramError> {
        let discriminator = *data.first().ok_or(EscrowError::InvalidInstruction)?;

        match discriminator {
            FUND_DISCRIMINATOR if data.len() == FUND_DATA_LEN => Ok(Self::Fund(FundArgs {
                bounty_id: array(data, 1)?,
                amount_lamports: u64::from_le_bytes(array(data, 33)?),
                offer_expires_at: i64::from_le_bytes(array(data, 41)?),
                acceptance_commitment: array(data, 49)?,
                state_bump: data[81],
                vault_bump: data[82],
                guard_bump: data[83],
            })),
            BIND_DISCRIMINATOR if data.len() == BIND_DATA_LEN => Ok(Self::Bind(BindArgs {
                bounty_id: array(data, 1)?,
                claimant: Pubkey::new_from_array(array(data, 33)?),
                claim_expires_at: i64::from_le_bytes(array(data, 65)?),
                claim_commitment: array(data, 73)?,
            })),
            RELEASE_DISCRIMINATOR if data.len() == RESOLVE_DATA_LEN => {
                Ok(Self::Release(ResolveArgs {
                    bounty_id: array(data, 1)?,
                    resolution_evidence: array(data, 33)?,
                }))
            }
            REFUND_DISCRIMINATOR if data.len() == RESOLVE_DATA_LEN => {
                Ok(Self::Refund(ResolveArgs {
                    bounty_id: array(data, 1)?,
                    resolution_evidence: array(data, 33)?,
                }))
            }
            _ => Err(EscrowError::InvalidInstruction.into()),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Fund(args) => {
                let mut data = Vec::with_capacity(FUND_DATA_LEN);
                data.push(FUND_DISCRIMINATOR);
                data.extend_from_slice(&args.bounty_id);
                data.extend_from_slice(&args.amount_lamports.to_le_bytes());
                data.extend_from_slice(&args.offer_expires_at.to_le_bytes());
                data.extend_from_slice(&args.acceptance_commitment);
                data.push(args.state_bump);
                data.push(args.vault_bump);
                data.push(args.guard_bump);
                data
            }
            Self::Bind(args) => {
                let mut data = Vec::with_capacity(BIND_DATA_LEN);
                data.push(BIND_DISCRIMINATOR);
                data.extend_from_slice(&args.bounty_id);
                data.extend_from_slice(args.claimant.as_ref());
                data.extend_from_slice(&args.claim_expires_at.to_le_bytes());
                data.extend_from_slice(&args.claim_commitment);
                data
            }
            Self::Release(args) => encode_resolve(RELEASE_DISCRIMINATOR, args),
            Self::Refund(args) => encode_resolve(REFUND_DISCRIMINATOR, args),
        }
    }
}

fn encode_resolve(discriminator: u8, args: &ResolveArgs) -> Vec<u8> {
    let mut data = Vec::with_capacity(RESOLVE_DATA_LEN);
    data.push(discriminator);
    data.extend_from_slice(&args.bounty_id);
    data.extend_from_slice(&args.resolution_evidence);
    data
}

fn array<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N], ProgramError> {
    data.get(offset..offset + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| EscrowError::InvalidInstruction.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> Vec<EscrowInstruction> {
        vec![
            EscrowInstruction::Fund(FundArgs {
                bounty_id: [1; 32],
                amount_lamports: 42,
                offer_expires_at: 900,
                acceptance_commitment: [2; 32],
                state_bump: 253,
                vault_bump: 252,
                guard_bump: 251,
            }),
            EscrowInstruction::Bind(BindArgs {
                bounty_id: [1; 32],
                claimant: Pubkey::new_from_array([3; 32]),
                claim_expires_at: 1_200,
                claim_commitment: [4; 32],
            }),
            EscrowInstruction::Release(ResolveArgs {
                bounty_id: [1; 32],
                resolution_evidence: [5; 32],
            }),
            EscrowInstruction::Refund(ResolveArgs {
                bounty_id: [1; 32],
                resolution_evidence: [6; 32],
            }),
        ]
    }

    #[test]
    fn codec_is_round_trip_and_length_exact() {
        let lengths = [
            FUND_DATA_LEN,
            BIND_DATA_LEN,
            RESOLVE_DATA_LEN,
            RESOLVE_DATA_LEN,
        ];
        for (instruction, expected_len) in fixtures().into_iter().zip(lengths) {
            let encoded = instruction.encode();
            assert_eq!(encoded.len(), expected_len);
            assert_eq!(EscrowInstruction::decode(&encoded).unwrap(), instruction);
        }
    }

    #[test]
    fn codec_rejects_truncation_suffixes_and_unknown_discriminators() {
        for instruction in fixtures() {
            let encoded = instruction.encode();
            assert!(EscrowInstruction::decode(&encoded[..encoded.len() - 1]).is_err());
            let mut suffixed = encoded.clone();
            suffixed.push(0);
            assert!(EscrowInstruction::decode(&suffixed).is_err());
        }
        assert!(EscrowInstruction::decode(&[4]).is_err());
        assert!(EscrowInstruction::decode(&[]).is_err());
    }
}
