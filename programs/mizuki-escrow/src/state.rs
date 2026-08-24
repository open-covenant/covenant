use solana_program::{program_error::ProgramError, pubkey::Pubkey};
use solana_sha256_hasher::hashv;

use crate::error::EscrowError;

pub const STATE_MAGIC: [u8; 8] = *b"MZKESC1\0";
pub const VAULT_MAGIC: [u8; 8] = *b"MZKVLT1\0";
pub const GUARD_MAGIC: [u8; 8] = *b"MZKGRD1\0";
pub const STATE_VERSION: u8 = 1;
pub const STATE_LEN: usize = 236;
pub const VAULT_LEN: usize = 40;
pub const GUARD_LEN: usize = 108;
pub const STATE_COMMITMENT_DOMAIN: &[u8] = b"mizuki:escrow:state:v1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EscrowStatus {
    Funded = 1,
    Bound = 2,
    Released = 3,
    Refunded = 4,
}

impl TryFrom<u8> for EscrowStatus {
    type Error = ProgramError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Funded),
            2 => Ok(Self::Bound),
            3 => Ok(Self::Released),
            4 => Ok(Self::Refunded),
            _ => Err(EscrowError::InvalidState.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowState {
    pub status: EscrowStatus,
    pub state_bump: u8,
    pub vault_bump: u8,
    pub authority: Pubkey,
    pub claimant: Pubkey,
    pub bounty_id: [u8; 32],
    pub amount_lamports: u64,
    pub created_at: i64,
    pub offer_expires_at: i64,
    pub claim_expires_at: i64,
    pub acceptance_commitment: [u8; 32],
    pub claim_commitment: [u8; 32],
    pub resolution_evidence: [u8; 32],
}

impl EscrowState {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != STATE_LEN || data[..8] != STATE_MAGIC || data[8] != STATE_VERSION {
            return Err(EscrowError::InvalidState.into());
        }

        let state = Self {
            status: data[9].try_into()?,
            state_bump: data[10],
            vault_bump: data[11],
            authority: Pubkey::new_from_array(array(data, 12)?),
            claimant: Pubkey::new_from_array(array(data, 44)?),
            bounty_id: array(data, 76)?,
            amount_lamports: u64::from_le_bytes(array(data, 108)?),
            created_at: i64::from_le_bytes(array(data, 116)?),
            offer_expires_at: i64::from_le_bytes(array(data, 124)?),
            claim_expires_at: i64::from_le_bytes(array(data, 132)?),
            acceptance_commitment: array(data, 140)?,
            claim_commitment: array(data, 172)?,
            resolution_evidence: array(data, 204)?,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() != STATE_LEN {
            return Err(EscrowError::InvalidState.into());
        }
        self.validate()?;

        data.fill(0);
        data[..8].copy_from_slice(&STATE_MAGIC);
        data[8] = STATE_VERSION;
        data[9] = self.status as u8;
        data[10] = self.state_bump;
        data[11] = self.vault_bump;
        data[12..44].copy_from_slice(self.authority.as_ref());
        data[44..76].copy_from_slice(self.claimant.as_ref());
        data[76..108].copy_from_slice(&self.bounty_id);
        data[108..116].copy_from_slice(&self.amount_lamports.to_le_bytes());
        data[116..124].copy_from_slice(&self.created_at.to_le_bytes());
        data[124..132].copy_from_slice(&self.offer_expires_at.to_le_bytes());
        data[132..140].copy_from_slice(&self.claim_expires_at.to_le_bytes());
        data[140..172].copy_from_slice(&self.acceptance_commitment);
        data[172..204].copy_from_slice(&self.claim_commitment);
        data[204..236].copy_from_slice(&self.resolution_evidence);
        Ok(())
    }

    pub fn commitment(&self) -> Result<[u8; 32], ProgramError> {
        let mut data = [0; STATE_LEN];
        self.pack(&mut data)?;
        Ok(hashv(&[STATE_COMMITMENT_DOMAIN, &data]).to_bytes())
    }

    fn validate(&self) -> Result<(), ProgramError> {
        if self.authority == Pubkey::default()
            || self.bounty_id == [0; 32]
            || self.amount_lamports == 0
            || self.offer_expires_at <= self.created_at
            || self.acceptance_commitment == [0; 32]
        {
            return Err(EscrowError::InvalidState.into());
        }

        let claimant_is_empty = self.claimant == Pubkey::default();
        let claim_is_empty = self.claim_expires_at == 0 && self.claim_commitment == [0; 32];
        let resolution_is_empty = self.resolution_evidence == [0; 32];

        match self.status {
            EscrowStatus::Funded if claimant_is_empty && claim_is_empty && resolution_is_empty => {
                Ok(())
            }
            EscrowStatus::Bound
                if !claimant_is_empty
                    && self.claim_expires_at > self.created_at
                    && self.claim_commitment != [0; 32]
                    && resolution_is_empty =>
            {
                Ok(())
            }
            EscrowStatus::Released
                if !claimant_is_empty
                    && self.claim_expires_at > self.created_at
                    && self.claim_commitment != [0; 32]
                    && !resolution_is_empty =>
            {
                Ok(())
            }
            EscrowStatus::Refunded
                if !resolution_is_empty
                    && ((claimant_is_empty && claim_is_empty)
                        || (!claimant_is_empty
                            && self.claim_expires_at > self.created_at
                            && self.claim_commitment != [0; 32])) =>
            {
                Ok(())
            }
            _ => Err(EscrowError::InvalidState.into()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowGuard {
    pub status: EscrowStatus,
    pub bump: u8,
    pub authority: Pubkey,
    pub bounty_id: [u8; 32],
    pub state_commitment: [u8; 32],
}

impl EscrowGuard {
    pub fn unpack(data: &[u8]) -> Result<Self, ProgramError> {
        if data.len() != GUARD_LEN
            || data[..8] != GUARD_MAGIC
            || data[8] != STATE_VERSION
            || data[11] != 0
        {
            return Err(EscrowError::InvalidState.into());
        }
        let guard = Self {
            status: data[9].try_into()?,
            bump: data[10],
            authority: Pubkey::new_from_array(array(data, 12)?),
            bounty_id: array(data, 44)?,
            state_commitment: array(data, 76)?,
        };
        if guard.authority == Pubkey::default()
            || guard.bounty_id == [0; 32]
            || guard.state_commitment == [0; 32]
        {
            return Err(EscrowError::InvalidState.into());
        }
        Ok(guard)
    }

    pub fn pack(&self, data: &mut [u8]) -> Result<(), ProgramError> {
        if data.len() != GUARD_LEN
            || self.authority == Pubkey::default()
            || self.bounty_id == [0; 32]
            || self.state_commitment == [0; 32]
        {
            return Err(EscrowError::InvalidState.into());
        }
        data.fill(0);
        data[..8].copy_from_slice(&GUARD_MAGIC);
        data[8] = STATE_VERSION;
        data[9] = self.status as u8;
        data[10] = self.bump;
        data[12..44].copy_from_slice(self.authority.as_ref());
        data[44..76].copy_from_slice(&self.bounty_id);
        data[76..108].copy_from_slice(&self.state_commitment);
        Ok(())
    }
}

pub fn pack_vault(state: &Pubkey, data: &mut [u8]) -> Result<(), ProgramError> {
    if data.len() != VAULT_LEN {
        return Err(EscrowError::InvalidVault.into());
    }
    data[..8].copy_from_slice(&VAULT_MAGIC);
    data[8..].copy_from_slice(state.as_ref());
    Ok(())
}

pub fn validate_vault(state: &Pubkey, data: &[u8]) -> Result<(), ProgramError> {
    if data.len() != VAULT_LEN || data[..8] != VAULT_MAGIC || data[8..] != state.to_bytes() {
        return Err(EscrowError::InvalidVault.into());
    }
    Ok(())
}

fn array<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N], ProgramError> {
    data.get(offset..offset + N)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| EscrowError::InvalidState.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn funded() -> EscrowState {
        EscrowState {
            status: EscrowStatus::Funded,
            state_bump: 254,
            vault_bump: 253,
            authority: Pubkey::new_unique(),
            claimant: Pubkey::default(),
            bounty_id: [1; 32],
            amount_lamports: 50,
            created_at: 100,
            offer_expires_at: 200,
            claim_expires_at: 0,
            acceptance_commitment: [2; 32],
            claim_commitment: [0; 32],
            resolution_evidence: [0; 32],
        }
    }

    #[test]
    fn state_layout_round_trips() {
        let state = funded();
        let mut data = [0; STATE_LEN];
        state.pack(&mut data).unwrap();
        assert_eq!(&data[..8], &STATE_MAGIC);
        assert_eq!(EscrowState::unpack(&data).unwrap(), state);
    }

    #[test]
    fn invalid_lifecycle_shapes_are_rejected() {
        let mut data = [0; STATE_LEN];
        let mut state = funded();
        state.status = EscrowStatus::Bound;
        assert!(state.pack(&mut data).is_err());

        state.claimant = Pubkey::new_unique();
        state.claim_expires_at = 300;
        state.claim_commitment = [3; 32];
        state.pack(&mut data).unwrap();

        data[9] = EscrowStatus::Released as u8;
        assert!(EscrowState::unpack(&data).is_err());
        data[8] = 2;
        assert!(EscrowState::unpack(&data).is_err());
    }

    #[test]
    fn vault_layout_binds_state() {
        let state = Pubkey::new_unique();
        let mut data = [0; VAULT_LEN];
        pack_vault(&state, &mut data).unwrap();
        validate_vault(&state, &data).unwrap();
        assert!(validate_vault(&Pubkey::new_unique(), &data).is_err());
    }

    #[test]
    fn guard_commits_full_state() {
        let state = funded();
        let guard = EscrowGuard {
            status: state.status,
            bump: 252,
            authority: state.authority,
            bounty_id: state.bounty_id,
            state_commitment: state.commitment().unwrap(),
        };
        let mut data = [0; GUARD_LEN];
        guard.pack(&mut data).unwrap();
        assert_eq!(EscrowGuard::unpack(&data).unwrap(), guard);

        let mut changed = state;
        changed.amount_lamports += 1;
        assert_ne!(changed.commitment().unwrap(), guard.state_commitment);
    }
}
