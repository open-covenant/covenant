//! Per-second GPU lease metering, on-chain.
//!
//! A GPU lease bills by the second, which is one state update per second
//! per session. That is the wrong shape for Solana L1 — the fees and the
//! latency both dominate the thing being billed — so every other metered
//! compute product settles once at the end and asks the renter to trust
//! the meter in between.
//!
//! This program puts the meter itself on-chain by running it on a
//! MagicBlock Ephemeral Rollup, and splits a lease across two accounts so
//! that the rollup only ever holds the part it needs to write:
//!
//! - `LeaseTerms` never leaves L1. Who the parties are, the rate, the
//!   window, the escrowed balance and the payout flags all live here, and
//!   only this program can write them.
//! - `LeaseMeter` is the account that gets delegated. It holds elapsed
//!   time and a provenance hash-chain, and nothing else.
//!
//! The lifecycle:
//!
//! 1. `open_lease` (L1) — the renter escrows the whole window
//!    (`rate x max_duration`) into a program-owned vault and names the
//!    two parties they are trusting with the meter: the coordinator that
//!    will observe the session, and the rollup validator that will host
//!    it. Nobody can move the escrow except through the settlement
//!    instructions below.
//! 2. `delegate_lease` — the coordinator hands the meter to the pinned
//!    validator. The escrow stays on L1 and stays this program's.
//! 3. `tick` (ER, gasless) — the coordinator pushes the cumulative
//!    elapsed and folds the tick's receipt hash into the provenance
//!    chain. A 600-second lease can be metered 600 times for nothing, and
//!    the chain of ticks is replayable.
//! 4. `undelegate_lease` — closes the meter and commits it back to L1.
//! 5. `settle_lease` (L1) — recomputes the charge from the terms and the
//!    committed elapsed, pays the operator, returns the rest to the
//!    renter.
//!
//! What the split buys: whoever holds the pinned validator identity
//! authors the bytes that get committed back to L1. Keeping the payout
//! addresses, the rate and the escrow balance out of the delegated
//! account bounds that host to overstating elapsed time, which the escrow
//! already caps, instead of letting it name itself as the operator and
//! take the vault.
//!
//! Who can do what, and why:
//!
//! - The **coordinator** meters. It is the only key that can tick,
//!   delegate, conclude or void, and the renter agrees to it by name when
//!   they escrow. That trust is the product: a renter must not be able to
//!   under-report their own usage and an operator must not be able to
//!   over-report it, so a third party observes and both sides accept who
//!   it is up front.
//! - **Settlement is permissionless once the lease is over** — the meter
//!   came back from the rollup, the lease was voided, or the window the
//!   renter paid for has elapsed. Neither party can strand the other's
//!   money by declining to push the button, and neither can cut the meter
//!   short while the session the renter paid for is still running.
//!
//! What this does NOT prove: which physical GPU ran the work. The ER
//! attests its own execution, not the machine. Verifiable metering and
//! verifiable settlement are the claims; "proven compute" is not.

use anchor_lang::prelude::*;
use anchor_spl::token_interface::{
    self, CloseAccount, Mint, TokenAccount, TokenInterface, TransferChecked,
};
use ephemeral_rollups_sdk::anchor::{commit, delegate, ephemeral};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::MagicIntentBundleBuilder;

declare_id!("CLSeVNrRi4TpXsXAkAuLh58kGCCAd1w1bj2CcEhTEESd");

/// A lease may not reserve more than a day. Bounds how much of a
/// renter's balance one signature can lock, and keeps the meter's
/// arithmetic far from overflow.
pub const MAX_DURATION_SECS: u64 = 86_400;
/// Genesis of the provenance hash-chain.
pub const PROVENANCE_GENESIS: [u8; 32] = [0u8; 32];

#[ephemeral]
#[program]
pub mod covenant_compute_lease {
    use super::*;

    /// Opens a lease and escrows the whole window in one step.
    ///
    /// The renter signs, and in signing names the coordinator that may
    /// meter them and the rollup validator that may host that meter.
    /// Both are recorded, so a buyer reading the chain can see who was
    /// authorised before a single second was billed, and neither can be
    /// swapped afterwards.
    ///
    /// The renter is part of the lease address. Without that, anyone who
    /// learned a job id — the assigned operator learns it at dispatch —
    /// could occupy the address first with terms of their own and the
    /// real open would fail for good, quietly downgrading the session to
    /// an off-chain meter.
    pub fn open_lease(
        ctx: Context<OpenLease>,
        job_id: [u8; 16],
        rate_micro_usdc_per_sec: u64,
        max_duration_secs: u64,
    ) -> Result<()> {
        require!(rate_micro_usdc_per_sec > 0, LeaseError::ZeroRate);
        require!(
            max_duration_secs > 0 && max_duration_secs <= MAX_DURATION_SECS,
            LeaseError::BadDuration
        );
        let window = rate_micro_usdc_per_sec
            .checked_mul(max_duration_secs)
            .ok_or(LeaseError::Overflow)?;

        // Escrow the whole window up front. Metering only ever decides
        // how this is split later; it can never ask for more.
        token_interface::transfer_checked(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                TransferChecked {
                    from: ctx.accounts.renter_tokens.to_account_info(),
                    mint: ctx.accounts.mint.to_account_info(),
                    to: ctx.accounts.vault.to_account_info(),
                    authority: ctx.accounts.renter.to_account_info(),
                },
            ),
            window,
            ctx.accounts.mint.decimals,
        )?;

        // Record what the vault actually received, not what was asked
        // for. A mint that skims a transfer fee delivers less than the
        // window, and a settlement that tried to move the requested
        // figure back out would revert on every attempt, freezing the
        // escrow for the life of the lease.
        ctx.accounts.vault.reload()?;
        let funded = ctx.accounts.vault.amount;
        require!(funded > 0, LeaseError::EmptyDeposit);

        let opened_at = Clock::get()?.unix_timestamp;
        let terms = &mut ctx.accounts.terms;
        terms.job_id = job_id;
        terms.renter = ctx.accounts.renter.key();
        terms.operator = ctx.accounts.operator.key();
        terms.coordinator = ctx.accounts.coordinator.key();
        terms.er_validator = ctx.accounts.er_validator.key();
        terms.mint = ctx.accounts.mint.key();
        terms.rate_micro_usdc_per_sec = rate_micro_usdc_per_sec;
        terms.max_duration_secs = max_duration_secs;
        terms.funded_micro_usdc = funded;
        terms.opened_at = opened_at;
        terms.paid_operator = false;
        terms.paid_renter = false;
        terms.voided = false;
        terms.delegated = false;
        terms.bump = ctx.bumps.terms;
        terms.meter_bump = ctx.bumps.meter;
        terms.vault_bump = ctx.bumps.vault;

        let meter = &mut ctx.accounts.meter;
        meter.terms = ctx.accounts.terms.key();
        meter.coordinator = ctx.accounts.coordinator.key();
        meter.job_id = job_id;
        meter.metered_ms = 0;
        meter.provenance_root = PROVENANCE_GENESIS;
        meter.concluded = false;
        meter.bump = ctx.bumps.meter;

        emit!(LeaseOpened {
            job_id,
            renter: ctx.accounts.renter.key(),
            operator: ctx.accounts.operator.key(),
            coordinator: ctx.accounts.coordinator.key(),
            er_validator: ctx.accounts.er_validator.key(),
            mint: ctx.accounts.mint.key(),
            rate_micro_usdc_per_sec,
            max_duration_secs,
            funded_micro_usdc: funded,
            opened_at,
        });
        Ok(())
    }

    /// Records the seconds served so far and folds the tick's receipt
    /// hash into the provenance chain. Runs in the ER, so a per-second
    /// meter costs nothing to keep.
    ///
    /// `metered_ms` is cumulative rather than a delta: a lost or
    /// duplicated tick then costs nothing, because settlement is always
    /// recomputed from the total elapsed. A tick that would go backwards
    /// is refused — the meter only ever moves forward.
    ///
    /// No money is computed here. The rollup host can rewrite anything in
    /// this account when it commits, so the rate and the escrow are kept
    /// on L1 and the charge is derived there at settlement.
    pub fn tick(ctx: Context<Tick>, metered_ms: u64, receipt_hash: [u8; 32]) -> Result<()> {
        let meter = &mut ctx.accounts.meter;
        require!(!meter.concluded, LeaseError::MeterClosed);
        require!(
            metered_ms >= meter.metered_ms,
            LeaseError::MeterWentBackwards
        );

        meter.metered_ms = metered_ms;
        meter.provenance_root =
            anchor_lang::solana_program::hash::hashv(&[&meter.provenance_root, &receipt_hash])
                .to_bytes();

        emit!(LeaseTicked {
            job_id: meter.job_id,
            metered_ms,
            receipt_hash,
            provenance_root: meter.provenance_root,
        });
        Ok(())
    }

    /// Pays the operator what the meter says and returns the rest to the
    /// renter, in one transaction.
    ///
    /// Permissionless, but only once the lease is actually over: the
    /// meter came back from the rollup, the lease was voided, or the
    /// window the renter escrowed has elapsed. An unconditional door here
    /// would let a renter settle at zero in the slot after the open,
    /// before the coordinator's delegate lands, and keep a full session
    /// of compute for nothing.
    pub fn settle_lease(ctx: Context<SettleLease>) -> Result<()> {
        let settlement = settlement_figures(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(
            !(ctx.accounts.terms.paid_operator && ctx.accounts.terms.paid_renter),
            LeaseError::AlreadySettled
        );

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let signer = &[seeds];
        let decimals = ctx.accounts.mint.decimals;

        let charged = if ctx.accounts.terms.paid_operator {
            0
        } else {
            settlement.charged.min(ctx.accounts.vault.amount)
        };
        if charged > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.operator_tokens.to_account_info(),
                        authority: ctx.accounts.terms.to_account_info(),
                    },
                    signer,
                ),
                charged,
                decimals,
            )?;
        }

        // Whatever is left is the renter's, including anything a third
        // party sent to the vault after the open — the vault is closed
        // below, so a residue would otherwise be locked behind an
        // authority that will never sign again.
        ctx.accounts.vault.reload()?;
        let refund = if ctx.accounts.terms.paid_renter {
            0
        } else {
            ctx.accounts.vault.amount
        };
        if refund > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.renter_tokens.to_account_info(),
                        authority: ctx.accounts.terms.to_account_info(),
                    },
                    signer,
                ),
                refund,
                decimals,
            )?;
        }

        ctx.accounts.vault.reload()?;
        if ctx.accounts.vault.amount == 0 {
            token_interface::close_account(CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                CloseAccount {
                    account: ctx.accounts.vault.to_account_info(),
                    destination: ctx.accounts.renter.to_account_info(),
                    authority: ctx.accounts.terms.to_account_info(),
                },
                signer,
            ))?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_operator = true;
        terms.paid_renter = true;
        emit!(LeaseSettled {
            job_id: terms.job_id,
            metered_ms: settlement.metered_ms,
            charged_micro_usdc: charged,
            refunded_micro_usdc: refund,
            provenance_root: settlement.provenance_root,
            voided: terms.voided,
        });
        Ok(())
    }

    /// Pays the operator's share on its own.
    ///
    /// The two payouts are separable because one blocked destination must
    /// not hold the other side's money. A settlement mint with a live
    /// freeze authority, or a party that simply closed its token account,
    /// would otherwise revert every settlement and strand the whole
    /// escrow rather than just that party's share.
    pub fn claim_operator_share(ctx: Context<ClaimOperatorShare>) -> Result<()> {
        let settlement = settlement_figures(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(!ctx.accounts.terms.paid_operator, LeaseError::AlreadyPaid);

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let amount = settlement.charged.min(ctx.accounts.vault.amount);
        if amount > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.operator_tokens.to_account_info(),
                        authority: ctx.accounts.terms.to_account_info(),
                    },
                    &[seeds],
                ),
                amount,
                ctx.accounts.mint.decimals,
            )?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_operator = true;
        emit!(LeaseOperatorPaid {
            job_id: terms.job_id,
            operator: terms.operator,
            metered_ms: settlement.metered_ms,
            amount_micro_usdc: amount,
        });
        Ok(())
    }

    /// Returns the renter's remainder on its own. The operator's share
    /// stays reserved in the vault until it is claimed, so calling this
    /// first cannot take the money out from under them.
    pub fn claim_renter_refund(ctx: Context<ClaimRenterRefund>) -> Result<()> {
        let settlement = settlement_figures(&ctx.accounts.terms, &ctx.accounts.meter)?;
        require!(!ctx.accounts.terms.paid_renter, LeaseError::AlreadyPaid);

        let renter = ctx.accounts.terms.renter;
        let job_id = ctx.accounts.terms.job_id;
        let bump = [ctx.accounts.terms.bump];
        let seeds: &[&[u8]] = &[b"lease", renter.as_ref(), job_id.as_ref(), &bump];
        let reserved = if ctx.accounts.terms.paid_operator {
            0
        } else {
            settlement.charged
        };
        let amount = ctx.accounts.vault.amount.saturating_sub(reserved);
        if amount > 0 {
            token_interface::transfer_checked(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    TransferChecked {
                        from: ctx.accounts.vault.to_account_info(),
                        mint: ctx.accounts.mint.to_account_info(),
                        to: ctx.accounts.renter_tokens.to_account_info(),
                        authority: ctx.accounts.terms.to_account_info(),
                    },
                    &[seeds],
                ),
                amount,
                ctx.accounts.mint.decimals,
            )?;
        }

        let terms = &mut ctx.accounts.terms;
        terms.paid_renter = true;
        emit!(LeaseRenterRefunded {
            job_id: terms.job_id,
            renter: terms.renter,
            amount_micro_usdc: amount,
        });
        Ok(())
    }

    /// Cancels the charge and opens settlement immediately: the renter
    /// gets the whole vault back and the operator gets nothing.
    ///
    /// The marketplace has terminal paths — a deadline that expired, a
    /// receipt that came back failed, an expired session swept — where
    /// the buyer is refunded in full off-chain. The meter is monotonic
    /// and cannot be wound back, so without this a lease that took ticks
    /// on one of those paths would still pay the operator on-chain for
    /// work the marketplace already refused to bill, and the escrow would
    /// sit funded until someone unwound it by hand.
    ///
    /// Coordinator-signed, which grants no power it did not already have:
    /// the party that decides what the meter says can already decide it
    /// says zero.
    pub fn void_lease(ctx: Context<VoidLease>) -> Result<()> {
        let terms = &mut ctx.accounts.terms;
        require!(
            !(terms.paid_operator && terms.paid_renter),
            LeaseError::AlreadySettled
        );
        require!(!terms.voided, LeaseError::AlreadyVoided);
        terms.voided = true;
        emit!(LeaseVoided {
            job_id: terms.job_id,
            coordinator: terms.coordinator,
        });
        Ok(())
    }

    /// Hands the meter to the rollup validator the renter pinned at open
    /// so ticks can run there.
    ///
    /// Coordinator-signed and validator-checked. Delegation is the act of
    /// giving an account to a third party that then writes its state
    /// back, so an open door here is an unauthenticated transfer of the
    /// session's meter to a host of the caller's choosing — including one
    /// that does not exist, which would leave the meter unreachable for
    /// the rest of the lease.
    #[cfg(feature = "ephemeral")]
    pub fn delegate_lease(ctx: Context<DelegateLease>) -> Result<()> {
        require!(!ctx.accounts.terms.voided, LeaseError::LeaseVoided);
        require!(
            !(ctx.accounts.terms.paid_operator && ctx.accounts.terms.paid_renter),
            LeaseError::AlreadySettled
        );
        let terms_key = ctx.accounts.terms.key();
        let validator = ctx.accounts.er_validator.key();
        ctx.accounts.delegate_meter(
            &ctx.accounts.payer,
            &[b"meter".as_ref(), terms_key.as_ref()],
            DelegateConfig {
                validator: Some(validator),
                ..Default::default()
            },
        )?;
        ctx.accounts.terms.delegated = true;
        Ok(())
    }

    /// Closes the meter and commits it back to L1, which is what makes
    /// settlement possible.
    ///
    /// The `concluded` flag rides the same commit. Without it the meter
    /// is writable again the moment it lands on L1, and the coordinator
    /// could raise the elapsed after the renter has already reconciled
    /// the committed figure and before anyone settles it.
    ///
    /// Coordinator-signed, for the same reason ticking is: ending the
    /// meter early is worth exactly as much as under-reporting it, so a
    /// renter must not be able to cut a live session's meter two seconds
    /// in and settle for one tick of a window they are still using.
    ///
    /// The meter arrives raw and is typed by hand. `commit_and_undelegate`
    /// hands the account to the delegation program inside this
    /// instruction, so Anchor's automatic write-back would land after the
    /// handover and the rollup rejects it as a write to an account this
    /// program no longer owns. Serializing before the CPI is also what
    /// puts `concluded` into the bytes that get committed.
    #[cfg(feature = "ephemeral")]
    pub fn undelegate_lease(ctx: Context<UndelegateLease>) -> Result<()> {
        let meter_info = ctx.accounts.meter.to_account_info();
        let mut meter = LeaseMeter::try_deserialize(&mut &meter_info.try_borrow_data()?[..])?;
        require_keys_eq!(
            meter.coordinator,
            ctx.accounts.coordinator.key(),
            ErrorCode::ConstraintHasOne
        );
        meter.concluded = true;

        let mut buf = Vec::with_capacity(8 + LeaseMeter::INIT_SPACE);
        meter.try_serialize(&mut buf)?;
        meter_info.try_borrow_mut_data()?[..buf.len()].copy_from_slice(&buf);

        MagicIntentBundleBuilder::new(
            ctx.accounts.payer.to_account_info(),
            ctx.accounts.magic_context.to_account_info(),
            ctx.accounts.magic_program.to_account_info(),
        )
        .commit_and_undelegate(&[meter_info])
        .build_and_invoke()?;
        Ok(())
    }
}

struct Settlement {
    metered_ms: u64,
    charged: u64,
    provenance_root: [u8; 32],
}

/// What the lease owes, computed on L1, and whether it may be paid yet.
///
/// The meter arrives as a raw account because a lease that is still
/// delegated is owned by the delegation program and cannot be
/// deserialized at all. That case is not an error: it is a meter whose
/// host never committed, which carries no evidence of time served, so the
/// charge is zero and the escrow goes home. Only the window expiring or a
/// void opens that door, and only the coordinator the renter named can
/// put a lease there in the first place.
fn settlement_figures(terms: &LeaseTerms, meter: &UncheckedAccount) -> Result<Settlement> {
    let committed = if meter.owner == &crate::ID {
        let data = meter.try_borrow_data()?;
        Some(LeaseMeter::try_deserialize(&mut &data[..])?)
    } else {
        None
    };

    let window_over = Clock::get()?.unix_timestamp
        >= terms
            .opened_at
            .saturating_add(terms.max_duration_secs as i64);
    // A meter this program owns again after a delegation has been through
    // the rollup and come back. Its own closed flag is not the test: that
    // flag rides a commit this program does not author, and a finished
    // lease should not have to wait out its whole window because one byte
    // went missing on the way home.
    let returned_from_rollup = terms.delegated && committed.is_some();
    require!(
        terms.voided || returned_from_rollup || window_over,
        LeaseError::LeaseStillRunning
    );

    let metered_ms = committed.as_ref().map_or(0, |m| m.metered_ms);
    let provenance_root = committed
        .as_ref()
        .map_or(PROVENANCE_GENESIS, |m| m.provenance_root);

    Ok(Settlement {
        metered_ms,
        charged: charge_for(terms, metered_ms),
        provenance_root,
    })
}

/// Pro-rata to the millisecond, rounded up: a started second is a served
/// second.
///
/// Clamped at both the window the renter signed for and the balance the
/// vault actually holds. The first clamp is what bounds a rollup host that
/// commits an inflated elapsed; the second is what keeps a mint that
/// skimmed the deposit from making every settlement ask the vault for more
/// than it has and revert forever.
fn charge_for(terms: &LeaseTerms, metered_ms: u64) -> u64 {
    if terms.voided {
        return 0;
    }
    let rate = u128::from(terms.rate_micro_usdc_per_sec);
    let window = rate * u128::from(terms.max_duration_secs);
    ((rate * u128::from(metered_ms)).div_ceil(1_000))
        .min(window)
        .min(u128::from(terms.funded_micro_usdc)) as u64
}

#[derive(Accounts)]
#[instruction(job_id: [u8; 16])]
pub struct OpenLease<'info> {
    #[account(mut)]
    pub renter: Signer<'info>,
    /// CHECK: recorded as the payout destination's owner; never signs.
    pub operator: UncheckedAccount<'info>,
    /// CHECK: recorded as the only key that may meter this lease. The
    /// renter is agreeing here to who observes them, so it is named at
    /// open and fixed for the life of the lease.
    pub coordinator: UncheckedAccount<'info>,
    /// CHECK: recorded as the only rollup identity the meter may be
    /// delegated to.
    pub er_validator: UncheckedAccount<'info>,
    #[account(
        init,
        payer = renter,
        space = 8 + LeaseTerms::INIT_SPACE,
        seeds = [b"lease", renter.key().as_ref(), job_id.as_ref()],
        bump,
    )]
    pub terms: Account<'info, LeaseTerms>,
    #[account(
        init,
        payer = renter,
        space = 8 + LeaseMeter::INIT_SPACE,
        seeds = [b"meter", terms.key().as_ref()],
        bump,
    )]
    pub meter: Account<'info, LeaseMeter>,
    pub mint: InterfaceAccount<'info, Mint>,
    /// A PDA rather than an associated token account: only this program
    /// can create an account at this address, so the vault cannot be
    /// created ahead of the open to make the open fail.
    #[account(
        init,
        payer = renter,
        seeds = [b"vault", terms.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = terms,
        token::token_program = token_program,
    )]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = renter)]
    pub renter_tokens: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
    pub system_program: Program<'info, System>,
}

/// The meter tick, signed by the coordinator the renter named at open.
///
/// This is the whole authority check. The account is reachable by anyone
/// who can send a transaction to the rollup, and its address is derivable
/// from a job id the operator learns at dispatch, so without a signature
/// bound to the lease a stranger could drive the meter to the full
/// escrowed window and settle it.
#[derive(Accounts)]
pub struct Tick<'info> {
    #[account(mut, has_one = coordinator)]
    pub meter: Account<'info, LeaseMeter>,
    pub coordinator: Signer<'info>,
}

#[derive(Accounts)]
pub struct SettleLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint,
    )]
    pub terms: Account<'info, LeaseTerms>,
    /// CHECK: pinned to this lease by its seeds. Left raw because a meter
    /// that is still delegated is owned by the delegation program;
    /// `settlement_figures` reads it only when this program owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    /// CHECK: the renter's account, credited the vault's rent when the
    /// vault is closed. They paid it at open.
    #[account(mut, address = terms.renter)]
    pub renter: UncheckedAccount<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = terms.operator)]
    pub operator_tokens: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = terms.renter)]
    pub renter_tokens: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ClaimOperatorShare<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint,
    )]
    pub terms: Account<'info, LeaseTerms>,
    /// CHECK: pinned to this lease by its seeds; read only when this
    /// program owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = terms.operator)]
    pub operator_tokens: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct ClaimRenterRefund<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = mint,
    )]
    pub terms: Account<'info, LeaseTerms>,
    /// CHECK: pinned to this lease by its seeds; read only when this
    /// program owns it.
    #[account(seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
    pub mint: InterfaceAccount<'info, Mint>,
    #[account(mut, seeds = [b"vault", terms.key().as_ref()], bump = terms.vault_bump)]
    pub vault: InterfaceAccount<'info, TokenAccount>,
    #[account(mut, token::mint = mint, token::authority = terms.renter)]
    pub renter_tokens: InterfaceAccount<'info, TokenAccount>,
    pub token_program: Interface<'info, TokenInterface>,
}

#[derive(Accounts)]
pub struct VoidLease<'info> {
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = coordinator,
    )]
    pub terms: Account<'info, LeaseTerms>,
    pub coordinator: Signer<'info>,
}

#[cfg(feature = "ephemeral")]
#[delegate]
#[derive(Accounts)]
pub struct DelegateLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(
        mut,
        seeds = [b"lease", terms.renter.as_ref(), terms.job_id.as_ref()],
        bump = terms.bump,
        has_one = coordinator,
        has_one = er_validator,
    )]
    pub terms: Account<'info, LeaseTerms>,
    pub coordinator: Signer<'info>,
    /// CHECK: the rollup identity recorded at open; matched by `has_one`.
    pub er_validator: UncheckedAccount<'info>,
    /// CHECK: the lease's meter, pinned by its seeds. Raw because
    /// delegation zeroes the account and reassigns its owner, which a
    /// typed account would try to write back over.
    #[account(mut, del, seeds = [b"meter", terms.key().as_ref()], bump = terms.meter_bump)]
    pub meter: UncheckedAccount<'info>,
}

#[cfg(feature = "ephemeral")]
#[commit]
#[derive(Accounts)]
pub struct UndelegateLease<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: owned by this program and typed by hand in the handler,
    /// which also checks the coordinator. It cannot be an
    /// `Account<LeaseMeter>`: Anchor would write it back after the commit
    /// has already handed it to the delegation program.
    #[account(mut, owner = crate::ID)]
    pub meter: UncheckedAccount<'info>,
    pub coordinator: Signer<'info>,
}

/// Everything that decides money. Never delegated, so the rollup host
/// that writes the meter cannot name itself the operator, raise the rate,
/// or move the escrow.
#[account]
#[derive(InitSpace)]
pub struct LeaseTerms {
    /// The coordinator's job id, as raw uuid bytes — the same identifier
    /// the signed work receipt carries, so a reader can line the two up.
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub operator: Pubkey,
    /// The only key that may meter, delegate, conclude or void this
    /// lease. Named by the renter at open.
    pub coordinator: Pubkey,
    /// The only rollup identity the meter may be delegated to.
    pub er_validator: Pubkey,
    pub mint: Pubkey,
    pub rate_micro_usdc_per_sec: u64,
    pub max_duration_secs: u64,
    /// What the vault actually received at open, which is not
    /// necessarily what was asked for.
    pub funded_micro_usdc: u64,
    /// Unix seconds at open. The window ends `max_duration_secs` later,
    /// and that is what makes permissionless settlement safe.
    pub opened_at: i64,
    pub paid_operator: bool,
    pub paid_renter: bool,
    /// Set when the marketplace refused to bill the session at all: the
    /// charge becomes zero and the whole vault goes back to the renter.
    pub voided: bool,
    /// Set the first time the meter is handed to the rollup. A meter this
    /// program owns again after that has been through the rollup and come
    /// back, which is the second, independent signal that the lease is
    /// over.
    pub delegated: bool,
    pub bump: u8,
    pub meter_bump: u8,
    pub vault_bump: u8,
}

/// The part that runs in the rollup. Elapsed time and a hash chain, and
/// nothing a host could rewrite into a payout.
#[account]
#[derive(InitSpace)]
pub struct LeaseMeter {
    pub terms: Pubkey,
    /// Carried alongside the terms so a tick can be authenticated inside
    /// the rollup, where the terms account is not present.
    pub coordinator: Pubkey,
    pub job_id: [u8; 16],
    /// Cumulative session time the coordinator has observed.
    pub metered_ms: u64,
    /// `root = sha256(root || receipt_hash)` over every tick, genesis 32
    /// zero bytes. Committed to L1 with the elapsed, so the settled
    /// amount arrives with a replayable record of how it was reached.
    pub provenance_root: [u8; 32],
    /// Set by the undelegate that commits this meter to L1. A concluded
    /// meter takes no further ticks, so the figure a renter reconciles at
    /// commit time is the figure that settles.
    pub concluded: bool,
    pub bump: u8,
}

#[event]
pub struct LeaseOpened {
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub operator: Pubkey,
    pub coordinator: Pubkey,
    pub er_validator: Pubkey,
    pub mint: Pubkey,
    pub rate_micro_usdc_per_sec: u64,
    pub max_duration_secs: u64,
    pub funded_micro_usdc: u64,
    pub opened_at: i64,
}

#[event]
pub struct LeaseTicked {
    pub job_id: [u8; 16],
    pub metered_ms: u64,
    pub receipt_hash: [u8; 32],
    pub provenance_root: [u8; 32],
}

#[event]
pub struct LeaseSettled {
    pub job_id: [u8; 16],
    pub metered_ms: u64,
    pub charged_micro_usdc: u64,
    pub refunded_micro_usdc: u64,
    pub provenance_root: [u8; 32],
    pub voided: bool,
}

#[event]
pub struct LeaseOperatorPaid {
    pub job_id: [u8; 16],
    pub operator: Pubkey,
    pub metered_ms: u64,
    pub amount_micro_usdc: u64,
}

#[event]
pub struct LeaseRenterRefunded {
    pub job_id: [u8; 16],
    pub renter: Pubkey,
    pub amount_micro_usdc: u64,
}

#[event]
pub struct LeaseVoided {
    pub job_id: [u8; 16],
    pub coordinator: Pubkey,
}

#[error_code]
pub enum LeaseError {
    #[msg("a lease rate must be greater than zero")]
    ZeroRate,
    #[msg("a lease window must be between one second and a day")]
    BadDuration,
    #[msg("the lease window overflows")]
    Overflow,
    #[msg("the escrow deposit arrived empty")]
    EmptyDeposit,
    #[msg("this lease is already settled")]
    AlreadySettled,
    #[msg("this share of the lease is already paid")]
    AlreadyPaid,
    #[msg("this lease is already voided")]
    AlreadyVoided,
    #[msg("this lease is voided")]
    LeaseVoided,
    #[msg("a meter may only move forward")]
    MeterWentBackwards,
    #[msg("this meter is closed")]
    MeterClosed,
    #[msg("this lease cannot be settled until its meter is concluded or its window has elapsed")]
    LeaseStillRunning,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(rate: u64, max_duration_secs: u64, funded_micro_usdc: u64) -> LeaseTerms {
        LeaseTerms {
            job_id: [0u8; 16],
            renter: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            coordinator: Pubkey::new_unique(),
            er_validator: Pubkey::new_unique(),
            mint: Pubkey::new_unique(),
            rate_micro_usdc_per_sec: rate,
            max_duration_secs,
            funded_micro_usdc,
            opened_at: 1_700_000_000,
            paid_operator: false,
            paid_renter: false,
            voided: false,
            delegated: false,
            bump: 255,
            meter_bump: 254,
            vault_bump: 253,
        }
    }

    #[test]
    fn a_started_second_is_a_served_second() {
        let t = terms(100, 600, 60_000);
        assert_eq!(charge_for(&t, 0), 0);
        assert_eq!(charge_for(&t, 1), 1);
        assert_eq!(charge_for(&t, 1_000), 100);
        assert_eq!(charge_for(&t, 1_001), 101);
        assert_eq!(charge_for(&t, 12_500), 1_250);
    }

    #[test]
    fn an_inflated_elapsed_cannot_charge_past_the_window() {
        // The rollup host authors the committed elapsed, so the escrow
        // has to be the ceiling rather than the meter's word.
        let t = terms(100, 600, 60_000);
        assert_eq!(charge_for(&t, 600_000), 60_000);
        assert_eq!(charge_for(&t, 600_001), 60_000);
        assert_eq!(charge_for(&t, u64::MAX), 60_000);
    }

    #[test]
    fn a_short_deposit_caps_the_charge_at_what_the_vault_holds() {
        // A transfer-fee mint delivers less than the window. Settlement
        // must still be payable, so the charge follows the balance.
        let t = terms(100, 600, 59_000);
        assert_eq!(charge_for(&t, 600_000), 59_000);
        assert_eq!(charge_for(&t, u64::MAX), 59_000);
    }

    #[test]
    fn a_voided_lease_charges_nothing_however_long_it_ran() {
        let mut t = terms(100, 600, 60_000);
        t.voided = true;
        assert_eq!(charge_for(&t, 600_000), 0);
        assert_eq!(charge_for(&t, u64::MAX), 0);
    }

    #[test]
    fn the_widest_legal_lease_does_not_overflow() {
        let rate = u64::MAX / MAX_DURATION_SECS;
        let window = rate * MAX_DURATION_SECS;
        let t = terms(rate, MAX_DURATION_SECS, window);
        assert_eq!(charge_for(&t, u64::MAX), window);
        assert_eq!(charge_for(&t, MAX_DURATION_SECS * 1_000), window);
    }

    #[test]
    fn two_renters_cannot_collide_on_one_job_id() {
        // The renter is in the seeds so that learning a job id — which
        // the assigned operator does at dispatch — is not enough to
        // occupy the address the honest open derives.
        let job_id = [7u8; 16];
        let renter = Pubkey::new_unique();
        let squatter = Pubkey::new_unique();
        let (mine, _) =
            Pubkey::find_program_address(&[b"lease", renter.as_ref(), job_id.as_ref()], &crate::ID);
        let (theirs, _) = Pubkey::find_program_address(
            &[b"lease", squatter.as_ref(), job_id.as_ref()],
            &crate::ID,
        );
        assert_ne!(mine, theirs);
    }

    #[test]
    fn the_meter_and_the_vault_hang_off_the_lease() {
        let (terms_pda, _) = Pubkey::find_program_address(
            &[b"lease", Pubkey::new_unique().as_ref(), &[3u8; 16]],
            &crate::ID,
        );
        let (meter, _) = Pubkey::find_program_address(&[b"meter", terms_pda.as_ref()], &crate::ID);
        let (vault, _) = Pubkey::find_program_address(&[b"vault", terms_pda.as_ref()], &crate::ID);
        assert_ne!(meter, vault);
        assert_ne!(meter, terms_pda);
    }

    #[test]
    fn the_delegated_account_carries_no_money_fields() {
        // The rollup host can rewrite every byte of the meter when it
        // commits. Nothing that decides a payout may live there.
        let meter = LeaseMeter {
            terms: Pubkey::new_unique(),
            coordinator: Pubkey::new_unique(),
            job_id: [0u8; 16],
            metered_ms: 0,
            provenance_root: PROVENANCE_GENESIS,
            concluded: false,
            bump: 255,
        };
        assert_eq!(
            LeaseMeter::INIT_SPACE,
            32 + 32 + 16 + 8 + 32 + 1 + 1,
            "a field was added to the delegated account; check it cannot decide money"
        );
        assert_eq!(LeaseTerms::INIT_SPACE, 16 + 32 * 5 + 8 * 4 + 7);
        assert_eq!(meter.metered_ms, 0);
    }
}
