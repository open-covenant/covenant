use solana_program::program_error::ProgramError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum EscrowError {
    InvalidInstruction = 0,
    InvalidAccountCount = 1,
    AccountNotWritable = 2,
    InvalidSystemProgram = 3,
    InvalidClock = 4,
    InvalidRent = 5,
    InvalidPda = 6,
    AlreadyInitialized = 7,
    InvalidState = 8,
    InvalidAuthority = 9,
    InvalidClaimant = 10,
    ZeroAmount = 11,
    InvalidExpiry = 12,
    InvalidCommitment = 13,
    InvalidVault = 14,
    InsufficientVaultBalance = 15,
    ArithmeticOverflow = 16,
}

impl From<EscrowError> for ProgramError {
    fn from(error: EscrowError) -> Self {
        Self::Custom(error as u32)
    }
}
