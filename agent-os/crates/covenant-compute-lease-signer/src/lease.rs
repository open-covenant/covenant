//! Addresses, instructions and account layouts for the lease meter.
//!
//! The meter ships in two programs: standalone as `compute-lease`, and inside
//! the settlement program, where the protocol config fronts it. They differ in
//! three places only: the tick instruction's name, the config account that
//! `open_lease` and `delegate_lease` take in the settlement build, and error
//! numbering. Seeds and account layouts are the same, so one set of builders
//! serves both.

use sha2::{Digest, Sha256};
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;

pub const SYSTEM_PROGRAM: Pubkey = Pubkey::from_str_const("11111111111111111111111111111111");
pub const TOKEN_PROGRAM: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
pub const DELEGATION_PROGRAM: Pubkey =
    Pubkey::from_str_const("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");
// Exist only inside a rollup, which is why undelegate goes to the rollup
// endpoint and never to L1.
pub const MAGIC_PROGRAM: Pubkey =
    Pubkey::from_str_const("Magic11111111111111111111111111111111111111");
pub const MAGIC_CONTEXT: Pubkey =
    Pubkey::from_str_const("MagicContext1111111111111111111111111111111");
pub const MEMO_PROGRAM: Pubkey =
    Pubkey::from_str_const("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");
/// The memo program refuses an unsigned memo past this many bytes.
pub const MEMO_MAX_BYTES: usize = 566;

/// The coordinator's payout memo, written beside the operator's share so the
/// chain record of that transaction names the job and receipt it pays for.
pub fn memo(text: &str) -> Instruction {
    Instruction {
        program_id: MEMO_PROGRAM,
        accounts: vec![],
        data: text.as_bytes().to_vec(),
    }
}

/// The protocol's own cap on a lease window.
pub const MAX_DURATION_SECS: u64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Standalone,
    Settlement { config: Pubkey },
}

impl Dialect {
    fn tick(&self) -> &'static str {
        match self {
            Dialect::Standalone => "tick",
            Dialect::Settlement { .. } => "tick_lease",
        }
    }

    fn config(&self) -> Option<Pubkey> {
        match self {
            Dialect::Standalone => None,
            Dialect::Settlement { config } => Some(*config),
        }
    }
}

pub fn config_address(program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"config"], program).0
}

/// The three accounts one lease owns. The renter is in the seeds, so learning
/// a job id is not enough to occupy the address an honest open derives.
#[derive(Debug, Clone, Copy)]
pub struct Lease {
    pub program: Pubkey,
    pub renter: Pubkey,
    pub job_id: [u8; 16],
    pub terms: Pubkey,
    pub meter: Pubkey,
    pub vault: Pubkey,
}

impl Lease {
    pub fn derive(program: Pubkey, renter: Pubkey, job_id: [u8; 16]) -> Self {
        let terms = Pubkey::find_program_address(&[b"lease", renter.as_ref(), &job_id], &program).0;
        let meter = Pubkey::find_program_address(&[b"meter", terms.as_ref()], &program).0;
        let vault = Pubkey::find_program_address(&[b"vault", terms.as_ref()], &program).0;
        Self {
            program,
            renter,
            job_id,
            terms,
            meter,
            vault,
        }
    }

    pub fn open(
        &self,
        dialect: Dialect,
        parties: &Parties,
        renter_tokens: Pubkey,
        token_program: Pubkey,
        rate_per_sec: u64,
        max_duration_secs: u64,
    ) -> Instruction {
        let mut data = discriminator("global", "open_lease").to_vec();
        data.extend_from_slice(&self.job_id);
        data.extend_from_slice(&rate_per_sec.to_le_bytes());
        data.extend_from_slice(&max_duration_secs.to_le_bytes());
        let mut accounts: Vec<AccountMeta> = dialect
            .config()
            .map(|c| AccountMeta::new_readonly(c, false))
            .into_iter()
            .collect();
        accounts.extend([
            AccountMeta::new(self.renter, true),
            AccountMeta::new_readonly(parties.operator, false),
            AccountMeta::new_readonly(parties.coordinator, false),
            AccountMeta::new_readonly(parties.validator, false),
            AccountMeta::new(self.terms, false),
            AccountMeta::new(self.meter, false),
            AccountMeta::new_readonly(parties.mint, false),
            AccountMeta::new(self.vault, false),
            AccountMeta::new(renter_tokens, false),
            AccountMeta::new_readonly(token_program, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ]);
        self.instruction(accounts, data)
    }

    /// The `#[delegate]` macro splices the delegation buffer, record and
    /// metadata in front of the meter and appends the owner and delegation
    /// programs, which is the order below.
    pub fn delegate(
        &self,
        dialect: Dialect,
        payer: Pubkey,
        coordinator: Pubkey,
        validator: Pubkey,
    ) -> Instruction {
        let buffer =
            Pubkey::find_program_address(&[b"buffer", self.meter.as_ref()], &self.program).0;
        let record = Pubkey::find_program_address(
            &[b"delegation", self.meter.as_ref()],
            &DELEGATION_PROGRAM,
        )
        .0;
        let metadata = Pubkey::find_program_address(
            &[b"delegation-metadata", self.meter.as_ref()],
            &DELEGATION_PROGRAM,
        )
        .0;
        let mut accounts = vec![AccountMeta::new(payer, true)];
        accounts.extend(
            dialect
                .config()
                .map(|c| AccountMeta::new_readonly(c, false)),
        );
        accounts.extend([
            AccountMeta::new(self.terms, false),
            AccountMeta::new_readonly(coordinator, true),
            AccountMeta::new_readonly(validator, false),
            AccountMeta::new(buffer, false),
            AccountMeta::new(record, false),
            AccountMeta::new(metadata, false),
            AccountMeta::new(self.meter, false),
            AccountMeta::new_readonly(self.program, false),
            AccountMeta::new_readonly(DELEGATION_PROGRAM, false),
            AccountMeta::new_readonly(SYSTEM_PROGRAM, false),
        ]);
        self.instruction(accounts, discriminator("global", "delegate_lease").to_vec())
    }

    pub fn tick(
        &self,
        dialect: Dialect,
        coordinator: Pubkey,
        metered_ms: u64,
        receipt_hash: [u8; 32],
    ) -> Instruction {
        let mut data = discriminator("global", dialect.tick()).to_vec();
        data.extend_from_slice(&metered_ms.to_le_bytes());
        data.extend_from_slice(&receipt_hash);
        self.instruction(
            vec![
                AccountMeta::new(self.meter, false),
                AccountMeta::new_readonly(coordinator, true),
            ],
            data,
        )
    }

    pub fn undelegate(&self, payer: Pubkey, coordinator: Pubkey) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(self.meter, false),
                AccountMeta::new_readonly(coordinator, true),
                AccountMeta::new_readonly(MAGIC_PROGRAM, false),
                AccountMeta::new(MAGIC_CONTEXT, false),
            ],
            discriminator("global", "undelegate_lease").to_vec(),
        )
    }

    pub fn settle(
        &self,
        payer: Pubkey,
        mint: Pubkey,
        operator_tokens: Pubkey,
        renter_tokens: Pubkey,
        token_program: Pubkey,
    ) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(self.terms, false),
                AccountMeta::new_readonly(self.meter, false),
                AccountMeta::new(self.renter, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(operator_tokens, false),
                AccountMeta::new(renter_tokens, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            discriminator("global", "settle_lease").to_vec(),
        )
    }

    /// Pays the operator's share on its own, so that transaction moves
    /// money to exactly one wallet.
    pub fn claim_operator_share(
        &self,
        payer: Pubkey,
        mint: Pubkey,
        operator_tokens: Pubkey,
        token_program: Pubkey,
    ) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(self.terms, false),
                AccountMeta::new_readonly(self.meter, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(operator_tokens, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            discriminator("global", "claim_operator_share").to_vec(),
        )
    }

    /// Returns the renter's side without touching the operator's token
    /// account, for a voided lease whose operator has none.
    pub fn claim_renter_refund(
        &self,
        payer: Pubkey,
        mint: Pubkey,
        renter_tokens: Pubkey,
        token_program: Pubkey,
    ) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(payer, true),
                AccountMeta::new(self.terms, false),
                AccountMeta::new_readonly(self.meter, false),
                AccountMeta::new_readonly(mint, false),
                AccountMeta::new(self.vault, false),
                AccountMeta::new(renter_tokens, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            discriminator("global", "claim_renter_refund").to_vec(),
        )
    }

    pub fn void(&self, coordinator: Pubkey) -> Instruction {
        self.instruction(
            vec![
                AccountMeta::new(self.terms, false),
                AccountMeta::new_readonly(coordinator, true),
            ],
            discriminator("global", "void_lease").to_vec(),
        )
    }

    fn instruction(&self, accounts: Vec<AccountMeta>, data: Vec<u8>) -> Instruction {
        Instruction {
            program_id: self.program,
            accounts,
            data,
        }
    }
}

/// Everyone a lease names besides the renter.
#[derive(Debug, Clone, Copy)]
pub struct Parties {
    pub operator: Pubkey,
    pub coordinator: Pubkey,
    pub validator: Pubkey,
    pub mint: Pubkey,
}

pub fn discriminator(namespace: &str, name: &str) -> [u8; 8] {
    let digest = Sha256::digest(format!("{namespace}:{name}").as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

/// `LeaseTerms`. Stays on L1 for the life of the lease, so nothing in it can
/// be rewritten by the rollup host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terms {
    pub renter: Pubkey,
    pub operator: Pubkey,
    pub coordinator: Pubkey,
    pub validator: Pubkey,
    pub mint: Pubkey,
    pub rate_per_sec: u64,
    pub max_duration_secs: u64,
    pub funded: u64,
    pub paid_operator: bool,
    pub paid_renter: bool,
    pub voided: bool,
    pub delegated: bool,
}

impl Terms {
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        if data.len() < 223 || data[..8] != discriminator("account", "LeaseTerms") {
            return Err("account is not a LeaseTerms".into());
        }
        Ok(Self {
            renter: pubkey_at(data, 24),
            operator: pubkey_at(data, 56),
            coordinator: pubkey_at(data, 88),
            validator: pubkey_at(data, 120),
            mint: pubkey_at(data, 152),
            rate_per_sec: u64_at(data, 184),
            max_duration_secs: u64_at(data, 192),
            funded: u64_at(data, 200),
            paid_operator: data[216] == 1,
            paid_renter: data[217] == 1,
            voided: data[218] == 1,
            delegated: data[219] == 1,
        })
    }

    pub fn settled(&self) -> bool {
        self.paid_operator && self.paid_renter
    }

    /// What settlement pays the operator for `metered_ms`: pro rata to the
    /// millisecond, rounded up, clamped at the window and at what the vault
    /// received. The program's `lease_charge`, reproduced.
    pub fn charge(&self, metered_ms: u64) -> u64 {
        if self.voided {
            return 0;
        }
        let rate = u128::from(self.rate_per_sec);
        let window = rate * u128::from(self.max_duration_secs);
        (rate * u128::from(metered_ms))
            .div_ceil(1_000)
            .min(window)
            .min(u128::from(self.funded)) as u64
    }
}

/// `LeaseMeter`. The delegated half: elapsed time and a hash chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meter {
    pub coordinator: Pubkey,
    pub metered_ms: u64,
    pub provenance_root: [u8; 32],
    pub concluded: bool,
}

impl Meter {
    pub fn decode(data: &[u8]) -> Result<Self, String> {
        if data.len() < 130 || data[..8] != discriminator("account", "LeaseMeter") {
            return Err("account is not a LeaseMeter".into());
        }
        let mut provenance_root = [0u8; 32];
        provenance_root.copy_from_slice(&data[96..128]);
        Ok(Self {
            coordinator: pubkey_at(data, 40),
            metered_ms: u64_at(data, 88),
            provenance_root,
            concluded: data[128] == 1,
        })
    }
}

/// The balance of an SPL token account, which sits at the same offset under
/// both token programs.
pub fn token_amount(data: &[u8]) -> Option<u64> {
    (data.len() >= 72).then(|| u64_at(data, 64))
}

fn pubkey_at(data: &[u8], offset: usize) -> Pubkey {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&data[offset..offset + 32]);
    Pubkey::new_from_array(bytes)
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&data[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Pubkey {
        Pubkey::new_from_array([n; 32])
    }

    fn parties() -> Parties {
        Parties {
            operator: key(2),
            coordinator: key(3),
            validator: key(4),
            mint: key(5),
        }
    }

    #[test]
    fn discriminators_match_the_deployed_idl() {
        // Read out of programs/compute-lease/client/discriminators.json.
        assert_eq!(
            discriminator("global", "open_lease"),
            [187, 79, 139, 164, 14, 110, 255, 127]
        );
        assert_eq!(
            discriminator("global", "delegate_lease"),
            [208, 75, 250, 115, 2, 133, 112, 51]
        );
        assert_eq!(
            discriminator("global", "tick"),
            [92, 79, 44, 8, 101, 80, 63, 15]
        );
        assert_eq!(
            discriminator("global", "undelegate_lease"),
            [161, 95, 240, 230, 35, 21, 136, 242]
        );
        assert_eq!(
            discriminator("global", "settle_lease"),
            [80, 14, 40, 219, 201, 133, 236, 90]
        );
        assert_eq!(
            discriminator("global", "claim_renter_refund"),
            [127, 108, 137, 251, 222, 91, 39, 29]
        );
        assert_eq!(
            discriminator("global", "claim_operator_share"),
            [48, 94, 26, 230, 17, 207, 37, 172]
        );
        assert_eq!(
            discriminator("global", "void_lease"),
            [62, 166, 226, 18, 70, 55, 61, 3]
        );
        assert_eq!(
            discriminator("account", "LeaseTerms"),
            [120, 89, 222, 140, 162, 246, 170, 179]
        );
        assert_eq!(
            discriminator("account", "LeaseMeter"),
            [254, 152, 135, 84, 146, 89, 109, 232]
        );
    }

    #[test]
    fn open_takes_the_config_only_in_the_settlement_build() {
        let lease = Lease::derive(key(9), key(1), [7u8; 16]);
        let standalone = lease.open(
            Dialect::Standalone,
            &parties(),
            key(6),
            TOKEN_PROGRAM,
            10,
            60,
        );
        let settlement = lease.open(
            Dialect::Settlement { config: key(8) },
            &parties(),
            key(6),
            TOKEN_PROGRAM,
            10,
            60,
        );
        assert_eq!(standalone.accounts.len(), 11);
        assert_eq!(settlement.accounts.len(), 12);
        assert_eq!(settlement.accounts[0].pubkey, key(8));
        assert!(!settlement.accounts[0].is_writable);
        assert_eq!(settlement.accounts[1..], standalone.accounts[..]);
        assert!(standalone.accounts[0].is_signer && standalone.accounts[0].pubkey == key(1));
        assert_eq!(standalone.data.len(), 8 + 16 + 8 + 8);
        assert_eq!(&standalone.data[8..24], &[7u8; 16]);
        assert_eq!(
            u64::from_le_bytes(standalone.data[24..32].try_into().unwrap()),
            10
        );
        assert_eq!(
            u64::from_le_bytes(standalone.data[32..40].try_into().unwrap()),
            60
        );
    }

    #[test]
    fn delegate_puts_the_config_after_the_payer() {
        let lease = Lease::derive(key(9), key(1), [7u8; 16]);
        let ix = lease.delegate(
            Dialect::Settlement { config: key(8) },
            key(1),
            key(3),
            key(4),
        );
        assert_eq!(ix.accounts[0].pubkey, key(1));
        assert_eq!(ix.accounts[1].pubkey, key(8));
        assert_eq!(ix.accounts[2].pubkey, lease.terms);
        assert!(ix.accounts[3].is_signer && ix.accounts[3].pubkey == key(3));
        assert_eq!(ix.accounts[8].pubkey, lease.meter);
        assert_eq!(ix.accounts.len(), 12);
        let standalone = lease.delegate(Dialect::Standalone, key(1), key(3), key(4));
        assert_eq!(standalone.accounts.len(), 11);
    }

    #[test]
    fn tick_is_named_per_dialect_and_carries_the_cumulative_total() {
        let lease = Lease::derive(key(9), key(1), [7u8; 16]);
        let ix = lease.tick(
            Dialect::Settlement { config: key(8) },
            key(3),
            42_000,
            [1u8; 32],
        );
        assert_eq!(ix.data[..8], discriminator("global", "tick_lease"));
        assert_eq!(
            u64::from_le_bytes(ix.data[8..16].try_into().unwrap()),
            42_000
        );
        assert_eq!(&ix.data[16..48], &[1u8; 32]);
        let standalone = lease.tick(Dialect::Standalone, key(3), 42_000, [1u8; 32]);
        assert_eq!(standalone.data[..8], discriminator("global", "tick"));
    }

    fn terms_bytes(rate: u64, window: u64, funded: u64, flags: [bool; 4]) -> Vec<u8> {
        let mut data = discriminator("account", "LeaseTerms").to_vec();
        data.extend_from_slice(&[7u8; 16]);
        for n in 1..=5u8 {
            data.extend_from_slice(key(n).as_ref());
        }
        data.extend_from_slice(&rate.to_le_bytes());
        data.extend_from_slice(&window.to_le_bytes());
        data.extend_from_slice(&funded.to_le_bytes());
        data.extend_from_slice(&1_700_000_000i64.to_le_bytes());
        data.extend(flags.map(u8::from));
        data.extend_from_slice(&[255, 254, 253]);
        data
    }

    #[test]
    fn terms_decode_at_the_program_offsets() {
        let terms =
            Terms::decode(&terms_bytes(100, 600, 60_000, [false, true, false, true])).unwrap();
        assert_eq!(terms.renter, key(1));
        assert_eq!(terms.coordinator, key(3));
        assert_eq!(terms.validator, key(4));
        assert_eq!(terms.mint, key(5));
        assert_eq!(
            (terms.rate_per_sec, terms.max_duration_secs, terms.funded),
            (100, 600, 60_000)
        );
        assert!(!terms.paid_operator && terms.paid_renter && !terms.voided && terms.delegated);
        assert!(Terms::decode(&[0u8; 223]).is_err());
    }

    #[test]
    fn charge_rounds_up_and_clamps_like_the_program() {
        let terms = Terms::decode(&terms_bytes(100, 600, 60_000, [false; 4])).unwrap();
        assert_eq!(terms.charge(0), 0);
        assert_eq!(terms.charge(1), 1);
        assert_eq!(terms.charge(1_000), 100);
        assert_eq!(terms.charge(1_001), 101);
        assert_eq!(terms.charge(10_000_000), 60_000);
        let skimmed = Terms::decode(&terms_bytes(100, 600, 59_000, [false; 4])).unwrap();
        assert_eq!(skimmed.charge(10_000_000), 59_000);
        let voided =
            Terms::decode(&terms_bytes(100, 600, 60_000, [false, false, true, false])).unwrap();
        assert_eq!(voided.charge(10_000), 0);
    }

    #[test]
    fn the_operator_share_touches_no_renter_account() {
        let lease = Lease::derive(key(9), key(1), [7u8; 16]);
        let ix = lease.claim_operator_share(key(1), key(5), key(2), TOKEN_PROGRAM);
        let keys: Vec<Pubkey> = ix.accounts.iter().map(|a| a.pubkey).collect();
        assert_eq!(
            keys,
            vec![
                key(1),
                lease.terms,
                lease.meter,
                key(5),
                lease.vault,
                key(2),
                TOKEN_PROGRAM
            ]
        );
        assert!(ix.accounts[5].is_writable && !ix.accounts[5].is_signer);
    }

    #[test]
    fn a_memo_carries_its_text_and_needs_no_signer() {
        let ix = memo("compute-payout:v1:job:sig");
        assert_eq!(ix.program_id, MEMO_PROGRAM);
        assert!(ix.accounts.is_empty());
        assert_eq!(ix.data, b"compute-payout:v1:job:sig");
    }

    #[test]
    fn meter_decodes_elapsed_root_and_closed_flag() {
        let mut data = discriminator("account", "LeaseMeter").to_vec();
        data.extend_from_slice(key(9).as_ref());
        data.extend_from_slice(key(3).as_ref());
        data.extend_from_slice(&[7u8; 16]);
        data.extend_from_slice(&61_000u64.to_le_bytes());
        data.extend_from_slice(&[0xabu8; 32]);
        data.extend_from_slice(&[1, 250]);
        let meter = Meter::decode(&data).unwrap();
        assert_eq!(meter.coordinator, key(3));
        assert_eq!(meter.metered_ms, 61_000);
        assert_eq!(meter.provenance_root, [0xab; 32]);
        assert!(meter.concluded);
    }
}
