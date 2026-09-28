//! The four steps the coordinator asks for.
//!
//! Every step reads the chain before it writes, so running one twice finishes
//! the first run's work instead of repeating it. That is what lets the
//! coordinator retry a step whose outcome it never heard.

use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use solana_compute_budget_interface::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signer::{keypair::Keypair, Signer};
use solana_sdk::transaction::Transaction;
use spl_associated_token_account::get_associated_token_address_with_program_id;
use spl_associated_token_account::instruction::create_associated_token_account_idempotent;
use tokio::time::{sleep, Instant};

use crate::lease::{self, Dialect, Lease, Meter, Parties, Terms};
use crate::rpc::{Rpc, SendError};

const PRIORITY_MICRO_LAMPORTS: u64 = 100_000;
const L1_CONFIRM: Duration = Duration::from_secs(60);
const ROLLUP_CONFIRM: Duration = Duration::from_secs(20);
/// A tick pass walks every live lease in turn, so a tick waits briefly for a
/// fresh delegation and leaves the rest to the next pass.
const TICK_PICKUP: Duration = Duration::from_secs(5);
const CONCLUDE_PICKUP: Duration = Duration::from_secs(30);
const COMMIT_WAIT: Duration = Duration::from_secs(90);

/// Where a failed step leaves the money, in the coordinator's terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// The lease has not paid the operator for this step and no longer can,
    /// so the coordinator's own books stay the record of what is owed.
    NotSubmitted,
    /// Something that changes what the vault pays may have landed. Nothing
    /// else should move until someone reads the chain.
    MaybeSubmitted,
}

#[derive(Debug, Serialize)]
pub struct Failure {
    pub error: String,
    pub stage: Stage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

pub fn refused(error: impl std::fmt::Display) -> Failure {
    Failure {
        error: error.to_string(),
        stage: Stage::NotSubmitted,
        signature: None,
    }
}

fn unresolved(error: impl std::fmt::Display, signature: Option<String>) -> Failure {
    Failure {
        error: error.to_string(),
        stage: Stage::MaybeSubmitted,
        signature,
    }
}

/// What the coordinator writes on stdin. The envelope fields ride every step;
/// the rest belong to the step that needs them.
#[derive(Debug, Deserialize)]
pub struct Request {
    pub program_id: String,
    pub mint: String,
    pub er_validator: String,
    pub job_id: String,
    /// The buyer behind the lease. Informational: the vault is funded by the
    /// deployment's renter key, which is the only renter this process can sign
    /// as.
    #[serde(default)]
    pub renter: Option<String>,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub rate_micro_usdc_per_sec: Option<u64>,
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
    #[serde(default)]
    pub metered_ms: Option<u64>,
    #[serde(default)]
    pub receipt_hash_hex: Option<String>,
    /// The coordinator's payout memo for the job. Given, the operator is paid
    /// in a transaction of its own that carries it.
    #[serde(default)]
    pub payout_memo: Option<String>,
}

enum MeterHome {
    Missing,
    Delegated,
    OnL1(Meter),
}

pub struct Session {
    renter: Keypair,
    coordinator: Keypair,
    l1: Rpc,
    rollup: Rpc,
    lease: Lease,
    mint: Pubkey,
    validator: Pubkey,
}

impl Session {
    pub fn new(
        renter: Keypair,
        coordinator: Keypair,
        l1_url: &str,
        rollup_url: &str,
        request: &Request,
    ) -> Result<Self, Failure> {
        if renter.pubkey() == coordinator.pubkey() {
            return Err(refused(
                "the renter and coordinator keys are the same key; the program's separation between \
                 who funds a lease and who meters it depends on them differing",
            ));
        }
        let program = parse_pubkey("program_id", &request.program_id)?;
        let lease = Lease::derive(
            program,
            renter.pubkey(),
            parse_hex("job_id", &request.job_id)?,
        );
        Ok(Self {
            renter,
            coordinator,
            l1: Rpc::new(l1_url),
            rollup: Rpc::new(rollup_url),
            lease,
            mint: parse_pubkey("mint", &request.mint)?,
            validator: parse_pubkey("er_validator", &request.er_validator)?,
        })
    }

    /// Opens the lease and hands its meter to the rollup.
    ///
    /// Once the vault is funded the step reports success even if delegation
    /// failed: the lease exists, and the next tick delegates before it
    /// meters. Reporting failure there would make the coordinator forget a
    /// funded lease.
    pub async fn open(&self, request: &Request) -> Result<String, Failure> {
        let operator = parse_pubkey("operator", request.operator.as_deref().unwrap_or_default())?;
        let rate = request
            .rate_micro_usdc_per_sec
            .filter(|r| *r > 0)
            .ok_or_else(|| refused("rate_micro_usdc_per_sec must be positive"))?;
        let window = request
            .max_duration_secs
            .filter(|d| (1..=lease::MAX_DURATION_SECS).contains(d))
            .ok_or_else(|| {
                refused(format!(
                    "max_duration_secs must be 1..={}",
                    lease::MAX_DURATION_SECS
                ))
            })?;
        let escrow = rate
            .checked_mul(window)
            .ok_or_else(|| refused("rate times window overflows"))?;
        let parties = Parties {
            operator,
            coordinator: self.coordinator.pubkey(),
            validator: self.validator,
            mint: self.mint,
        };

        self.check_rollup_identity().await?;
        let dialect = self.dialect().await?;
        let token_program = self.token_program().await?;
        if let Some(buyer) = request.renter.as_deref() {
            eprintln!(
                "lease for buyer {buyer} escrowed by renter {}",
                self.lease.renter
            );
        }

        let mut opened = None;
        let terms = match self.terms().await? {
            Some(terms) => {
                check_terms(&terms, &parties, rate, window)?;
                terms
            }
            None => {
                let renter_tokens = self.token_account(&self.lease.renter, &token_program);
                let balance = self
                    .l1
                    .account(&renter_tokens)
                    .await
                    .map_err(refused)?
                    .and_then(|a| lease::token_amount(&a.data))
                    .unwrap_or(0);
                if balance < escrow {
                    return Err(refused(format!(
                        "renter {} holds {balance} of mint {}; the lease escrows {escrow}",
                        self.lease.renter, self.mint
                    )));
                }
                let ix = self.lease.open(
                    dialect,
                    &parties,
                    renter_tokens,
                    token_program,
                    rate,
                    window,
                );
                opened = Some(match self.send_l1(&[ix], 250_000, false).await {
                    Ok(signature) => signature,
                    Err(SendError::Refused(e)) => return Err(refused(format!("open_lease: {e}"))),
                    Err(SendError::Unknown { signature, message }) => {
                        if !matches!(self.terms().await, Ok(Some(_))) {
                            return Err(unresolved(
                                format!("open_lease: {message}"),
                                Some(signature),
                            ));
                        }
                        signature
                    }
                });
                self.terms().await?.ok_or_else(|| {
                    unresolved(
                        "open_lease confirmed but the lease is not readable yet",
                        opened.clone(),
                    )
                })?
            }
        };
        if terms.settled() || terms.voided {
            return Err(refused("the lease for this job has already concluded"));
        }
        if !terms.delegated {
            match self.delegate(dialect).await {
                Ok(signature) => return Ok(opened.unwrap_or(signature)),
                Err(e) => eprintln!(
                    "delegate_lease failed, the next tick retries it: {}",
                    e.error
                ),
            }
        }
        Ok(self.or_latest(opened).await)
    }

    /// Pushes one cumulative observation into the rollup.
    pub async fn tick(&self, request: &Request) -> Result<String, Failure> {
        let (metered_ms, receipt_hash) = observation(request)?;
        let dialect = self.dialect().await?;
        let terms = self
            .terms()
            .await?
            .ok_or_else(|| refused("no lease is open for this job"))?;
        if terms.settled() || terms.voided {
            return Err(refused("the lease has concluded"));
        }
        let meter = self
            .meter_in_rollup_or_delegate(dialect, &terms, TICK_PICKUP)
            .await?;
        if metered_ms < meter.metered_ms {
            // A tick overtaken by a later one; the total it carries is
            // already on the meter.
            return Ok(String::new());
        }
        let ix = self
            .lease
            .tick(dialect, self.coordinator.pubkey(), metered_ms, receipt_hash);
        self.send_rollup(&[ix]).await.map_err(|e| match e {
            SendError::Refused(m) => refused(format!("tick: {m}")),
            SendError::Unknown { signature, message } => {
                unresolved(format!("tick: {message}"), Some(signature))
            }
        })
    }

    /// Commits the meter at exactly the elapsed the coordinator bills and
    /// settles the vault.
    ///
    /// Success means the operator was paid on chain for that elapsed and
    /// nothing else. Anything short of that ends in `void_lease`: a meter
    /// that came home is claimable by anyone, so a lease that fails to settle
    /// here would otherwise still pay the operator after the coordinator has
    /// paid them itself.
    pub async fn conclude(&self, request: &Request) -> Result<String, Failure> {
        let (metered_ms, receipt_hash) = observation(request)?;
        let dialect = self.dialect().await?;
        let token_program = self.token_program().await?;
        let terms = self
            .terms()
            .await?
            .ok_or_else(|| refused("no lease is open for this job"))?;
        let payout_memo = request.payout_memo.as_deref();
        if terms.voided {
            self.refund_voided(&terms, token_program).await;
            return Err(refused("the lease was voided; nothing was paid on chain"));
        }
        if terms.paid_operator {
            return self
                .already_paid(&terms, metered_ms, payout_memo, token_program)
                .await;
        }
        match self
            .settle_on_chain(
                dialect,
                &terms,
                token_program,
                metered_ms,
                receipt_hash,
                payout_memo,
            )
            .await
        {
            Ok(signature) => Ok(signature),
            Err(failure) => {
                self.fall_back_to_void(failure, token_program, metered_ms, payout_memo)
                    .await
            }
        }
    }

    /// Zeroes the charge and returns the vault to the renter.
    pub async fn void(&self) -> Result<String, Failure> {
        let token_program = self.token_program().await?;
        let terms = self
            .terms()
            .await?
            .ok_or_else(|| refused("no lease is open for this job"))?;
        if terms.voided {
            self.refund_voided(&terms, token_program).await;
            return Ok(self.or_latest(None).await);
        }
        if terms.paid_operator {
            return Err(unresolved(
                "the lease already paid the operator on chain and can no longer be voided",
                self.l1
                    .latest_signature(&self.lease.terms)
                    .await
                    .ok()
                    .flatten(),
            ));
        }
        let ix = self.lease.void(self.coordinator.pubkey());
        let sent = self.send_l1(&[ix], 60_000, true).await;
        let terms = match self.terms().await {
            Ok(Some(terms)) => terms,
            _ => {
                return Err(match sent {
                    Ok(signature) => unresolved(
                        "void_lease landed but the lease could not be read back",
                        Some(signature),
                    ),
                    Err(e) => unresolved(format!("void_lease: {e}"), None),
                })
            }
        };
        if !terms.voided {
            return Err(match sent {
                Err(SendError::Refused(m)) => refused(format!("void_lease: {m}")),
                Err(SendError::Unknown { signature, message }) => {
                    unresolved(format!("void_lease: {message}"), Some(signature))
                }
                Ok(signature) => unresolved(
                    "void_lease confirmed but the lease reads unvoided",
                    Some(signature),
                ),
            });
        }
        if terms.paid_operator {
            return Err(unresolved(
                "the operator's share was claimed on chain before the void landed",
                sent.ok(),
            ));
        }
        self.refund_voided(&terms, token_program).await;
        Ok(match sent {
            Ok(signature) => signature,
            Err(_) => self.or_latest(None).await,
        })
    }

    async fn settle_on_chain(
        &self,
        dialect: Dialect,
        terms: &Terms,
        token_program: Pubkey,
        metered_ms: u64,
        receipt_hash: [u8; 32],
        payout_memo: Option<&str>,
    ) -> Result<String, Failure> {
        let committed = match self.meter_home().await? {
            MeterHome::OnL1(meter) if meter.concluded => meter,
            _ => {
                self.check_rollup_identity().await?;
                let meter = self
                    .meter_in_rollup_or_delegate(dialect, terms, CONCLUDE_PICKUP)
                    .await?;
                if meter.metered_ms > metered_ms {
                    return Err(refused(format!(
                        "the rollup meter is at {} ms, past the {metered_ms} ms being billed",
                        meter.metered_ms
                    )));
                }
                let tick =
                    self.lease
                        .tick(dialect, self.coordinator.pubkey(), metered_ms, receipt_hash);
                self.send_rollup(&[tick])
                    .await
                    .map_err(|e| refused(format!("final tick: {e}")))?;
                let undelegate = self
                    .lease
                    .undelegate(self.renter.pubkey(), self.coordinator.pubkey());
                match self.send_rollup(&[undelegate]).await {
                    Ok(_) => {}
                    Err(SendError::Refused(m)) => {
                        return Err(refused(format!("undelegate_lease: {m}")))
                    }
                    Err(e) => {
                        eprintln!("undelegate_lease outcome unknown ({e}); waiting for the commit")
                    }
                }
                self.await_commit().await?
            }
        };
        if committed.metered_ms != metered_ms {
            return Err(refused(format!(
                "the meter committed {} ms but the coordinator bills {metered_ms} ms",
                committed.metered_ms
            )));
        }
        if let Some(memo) = payout_memo {
            return self.pay_operator(terms, token_program, memo).await;
        }
        let operator_tokens = self.token_account(&terms.operator, &token_program);
        let renter_tokens = self.token_account(&self.lease.renter, &token_program);
        let create = create_associated_token_account_idempotent(
            &self.renter.pubkey(),
            &terms.operator,
            &self.mint,
            &token_program,
        );
        let settle = self.lease.settle(
            self.renter.pubkey(),
            self.mint,
            operator_tokens,
            renter_tokens,
            token_program,
        );
        match self.send_l1(&[create, settle], 200_000, false).await {
            Ok(signature) => Ok(signature),
            Err(SendError::Refused(m)) => Err(refused(format!("settle_lease: {m}"))),
            Err(SendError::Unknown { signature, message }) => match self.terms().await {
                Ok(Some(t)) if t.settled() && !t.voided => Ok(signature),
                _ => Err(unresolved(
                    format!("settle_lease: {message}"),
                    Some(signature),
                )),
            },
        }
    }

    /// Pays the operator's share in a transaction of its own that carries
    /// the coordinator's payout memo, then returns the renter's remainder.
    ///
    /// Split because a verifier reads a payout as one memo and one wallet
    /// credited; `settle_lease` credits both sides at once. The signature
    /// returned is the operator's payment.
    async fn pay_operator(
        &self,
        terms: &Terms,
        token_program: Pubkey,
        memo: &str,
    ) -> Result<String, Failure> {
        if memo.len() > lease::MEMO_MAX_BYTES {
            return Err(refused(format!(
                "payout memo is {} bytes; the memo program takes {}",
                memo.len(),
                lease::MEMO_MAX_BYTES
            )));
        }
        let operator_tokens = self.token_account(&terms.operator, &token_program);
        let create = create_associated_token_account_idempotent(
            &self.renter.pubkey(),
            &terms.operator,
            &self.mint,
            &token_program,
        );
        let claim = self.lease.claim_operator_share(
            self.renter.pubkey(),
            self.mint,
            operator_tokens,
            token_program,
        );
        let paid = match self
            .send_l1(&[lease::memo(memo), create, claim], 200_000, false)
            .await
        {
            Ok(signature) => signature,
            Err(SendError::Refused(m)) => {
                return Err(refused(format!("claim_operator_share: {m}")))
            }
            Err(SendError::Unknown { signature, message }) => match self.terms().await {
                Ok(Some(t)) if t.paid_operator && !t.voided => signature,
                _ => {
                    return Err(unresolved(
                        format!("claim_operator_share: {message}"),
                        Some(signature),
                    ))
                }
            },
        };
        self.return_remainder(terms, token_program).await;
        Ok(paid)
    }

    /// Settles what is left once the operator's share is out: the program
    /// charges nothing more, sends the rest to the renter and closes the
    /// vault. Best effort, since the remainder stays claimable by anyone.
    async fn return_remainder(&self, terms: &Terms, token_program: Pubkey) {
        let settle = self.lease.settle(
            self.renter.pubkey(),
            self.mint,
            self.token_account(&terms.operator, &token_program),
            self.token_account(&self.lease.renter, &token_program),
            token_program,
        );
        for attempt in 1..=2 {
            match self
                .send_l1(std::slice::from_ref(&settle), 150_000, false)
                .await
            {
                Ok(signature) => {
                    eprintln!("remainder returned to the renter in {signature}");
                    return;
                }
                Err(e) if attempt == 2 => {
                    eprintln!("remainder not returned ({e}); it stays claimable by the renter")
                }
                Err(_) => sleep(Duration::from_secs(2)).await,
            }
        }
    }

    /// The operator's share went out before this call. Finish returning the
    /// remainder and report that payment, if it was for the billed elapsed.
    async fn already_paid(
        &self,
        terms: &Terms,
        metered_ms: u64,
        payout_memo: Option<&str>,
        token_program: Pubkey,
    ) -> Result<String, Failure> {
        if !terms.paid_renter {
            self.return_remainder(terms, token_program).await;
        }
        let committed = match self.meter_home().await? {
            MeterHome::OnL1(meter) => meter.metered_ms,
            _ => 0,
        };
        let signature = match payout_memo {
            Some(memo) => self
                .l1
                .signature_with_memo(&self.lease.terms, memo)
                .await
                .ok()
                .flatten(),
            None => None,
        };
        let signature = match signature {
            Some(signature) => Some(signature),
            None => self
                .l1
                .latest_signature(&self.lease.terms)
                .await
                .ok()
                .flatten(),
        };
        if committed == metered_ms {
            return Ok(signature.unwrap_or_default());
        }
        Err(unresolved(
            format!(
                "the lease paid the operator on chain for {committed} ms ({}) but the coordinator bills {metered_ms} ms",
                terms.charge(committed)
            ),
            signature,
        ))
    }

    /// Voids a lease whose on-chain settlement did not complete, then reports
    /// what the operator can still be paid on chain: nothing, if the void
    /// landed before any payout.
    async fn fall_back_to_void(
        &self,
        failure: Failure,
        token_program: Pubkey,
        metered_ms: u64,
        payout_memo: Option<&str>,
    ) -> Result<String, Failure> {
        eprintln!(
            "on-chain settlement did not complete ({}); voiding the lease",
            failure.error
        );
        let ix = self.lease.void(self.coordinator.pubkey());
        let voided = match self.send_l1(&[ix], 60_000, true).await {
            Ok(signature) => Some(signature),
            Err(e) => {
                eprintln!("void_lease: {e}");
                None
            }
        };
        let terms = match self.terms().await {
            Ok(Some(terms)) => terms,
            _ => {
                return Err(unresolved(
                    format!(
                        "{}; the lease could not be read after voiding",
                        failure.error
                    ),
                    voided.or(failure.signature),
                ))
            }
        };
        if !terms.voided {
            if terms.paid_operator {
                return self
                    .already_paid(&terms, metered_ms, payout_memo, token_program)
                    .await;
            }
            return Err(unresolved(
                format!("{}; the void did not land", failure.error),
                failure.signature,
            ));
        }
        if terms.paid_operator {
            return Err(unresolved(
                format!(
                    "{}; the operator's share was claimed on chain before the void",
                    failure.error
                ),
                voided,
            ));
        }
        self.refund_voided(&terms, token_program).await;
        Err(refused(format!(
            "{}; the lease is voided on chain{}, so the operator is paid off chain",
            failure.error,
            voided.map(|s| format!(" in {s}")).unwrap_or_default()
        )))
    }

    /// Sends the vault back to the renter once the charge is zero. Best
    /// effort: a voided lease stays claimable by anyone, so a failure here
    /// strands nothing.
    async fn refund_voided(&self, terms: &Terms, token_program: Pubkey) {
        if terms.paid_renter {
            return;
        }
        let renter_tokens = self.token_account(&self.lease.renter, &token_program);
        let operator_tokens = self.token_account(&terms.operator, &token_program);
        // Settling closes the vault and returns its rent, but needs the
        // operator's token account to exist; the renter-only claim does not.
        let ix = match self.l1.account(&operator_tokens).await {
            Ok(Some(_)) => self.lease.settle(
                self.renter.pubkey(),
                self.mint,
                operator_tokens,
                renter_tokens,
                token_program,
            ),
            _ => self.lease.claim_renter_refund(
                self.renter.pubkey(),
                self.mint,
                renter_tokens,
                token_program,
            ),
        };
        match self.send_l1(&[ix], 120_000, false).await {
            Ok(signature) => eprintln!("vault returned to the renter in {signature}"),
            Err(e) => eprintln!("vault refund did not complete ({e}); it stays claimable"),
        }
    }

    async fn meter_in_rollup_or_delegate(
        &self,
        dialect: Dialect,
        terms: &Terms,
        pickup: Duration,
    ) -> Result<Meter, Failure> {
        match self.meter_home().await? {
            MeterHome::Missing => Err(refused("the lease has no meter account")),
            MeterHome::OnL1(meter) if meter.concluded => {
                Err(refused("the meter has already concluded"))
            }
            MeterHome::OnL1(_) if terms.delegated => Err(refused(
                "the meter came back from the rollup without concluding",
            )),
            MeterHome::OnL1(_) => {
                self.delegate(dialect).await?;
                self.await_pickup(pickup).await
            }
            MeterHome::Delegated => match self.meter_in_rollup().await.map_err(refused)? {
                Some(meter) => Ok(meter),
                None => self.await_pickup(pickup).await,
            },
        }
    }

    async fn delegate(&self, dialect: Dialect) -> Result<String, Failure> {
        let ix = self.lease.delegate(
            dialect,
            self.renter.pubkey(),
            self.coordinator.pubkey(),
            self.validator,
        );
        match self.send_l1(&[ix], 250_000, true).await {
            Ok(signature) => Ok(signature),
            Err(SendError::Refused(m)) => Err(refused(format!("delegate_lease: {m}"))),
            Err(SendError::Unknown { signature, message }) => match self.terms().await {
                Ok(Some(t)) if t.delegated => Ok(signature),
                _ => Err(unresolved(
                    format!("delegate_lease: {message}"),
                    Some(signature),
                )),
            },
        }
    }

    async fn await_pickup(&self, timeout: Duration) -> Result<Meter, Failure> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Ok(Some(meter)) = self.meter_in_rollup().await {
                return Ok(meter);
            }
            if Instant::now() >= deadline {
                return Err(refused("the rollup has not picked up the meter yet"));
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    async fn await_commit(&self) -> Result<Meter, Failure> {
        let deadline = Instant::now() + COMMIT_WAIT;
        loop {
            if let Ok(MeterHome::OnL1(meter)) = self.meter_home().await {
                if meter.concluded {
                    return Ok(meter);
                }
            }
            if Instant::now() >= deadline {
                return Err(unresolved(
                    format!(
                        "the meter did not come back to L1 within {}s",
                        COMMIT_WAIT.as_secs()
                    ),
                    None,
                ));
            }
            sleep(Duration::from_secs(1)).await;
        }
    }

    async fn dialect(&self) -> Result<Dialect, Failure> {
        match self
            .l1
            .account(&self.lease.program)
            .await
            .map_err(refused)?
        {
            Some(program) if program.executable => {}
            _ => {
                return Err(refused(format!(
                    "program {} is not deployed on this cluster",
                    self.lease.program
                )))
            }
        }
        let config = lease::config_address(&self.lease.program);
        Ok(match self.l1.account(&config).await.map_err(refused)? {
            Some(account) if account.owner == self.lease.program => Dialect::Settlement { config },
            _ => Dialect::Standalone,
        })
    }

    async fn check_rollup_identity(&self) -> Result<(), Failure> {
        let identity = self.rollup.identity().await.map_err(refused)?;
        if identity != self.validator.to_string() {
            return Err(refused(format!(
                "the rollup endpoint answers as {identity}, not the pinned validator {}",
                self.validator
            )));
        }
        Ok(())
    }

    async fn token_program(&self) -> Result<Pubkey, Failure> {
        let mint = self
            .l1
            .account(&self.mint)
            .await
            .map_err(refused)?
            .ok_or_else(|| refused(format!("mint {} does not exist", self.mint)))?;
        if mint.owner != lease::TOKEN_PROGRAM && mint.owner != lease::TOKEN_2022_PROGRAM {
            return Err(refused(format!("{} is not an SPL mint", self.mint)));
        }
        Ok(mint.owner)
    }

    async fn terms(&self) -> Result<Option<Terms>, Failure> {
        match self.l1.account(&self.lease.terms).await.map_err(refused)? {
            None => Ok(None),
            Some(account) if account.owner != self.lease.program => Err(refused(format!(
                "the lease address {} is owned by {}",
                self.lease.terms, account.owner
            ))),
            Some(account) => Terms::decode(&account.data).map(Some).map_err(refused),
        }
    }

    async fn meter_home(&self) -> Result<MeterHome, Failure> {
        match self.l1.account(&self.lease.meter).await.map_err(refused)? {
            None => Ok(MeterHome::Missing),
            Some(account) if account.owner == lease::DELEGATION_PROGRAM => Ok(MeterHome::Delegated),
            Some(account) if account.owner == self.lease.program => Meter::decode(&account.data)
                .map(MeterHome::OnL1)
                .map_err(refused),
            Some(account) => Err(refused(format!("the meter is owned by {}", account.owner))),
        }
    }

    async fn meter_in_rollup(&self) -> Result<Option<Meter>, String> {
        match self.rollup.account(&self.lease.meter).await? {
            Some(account) if account.owner == self.lease.program => {
                Meter::decode(&account.data).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn token_account(&self, owner: &Pubkey, token_program: &Pubkey) -> Pubkey {
        get_associated_token_address_with_program_id(owner, &self.mint, token_program)
    }

    async fn or_latest(&self, signature: Option<String>) -> String {
        match signature {
            Some(signature) => signature,
            None => self
                .l1
                .latest_signature(&self.lease.terms)
                .await
                .ok()
                .flatten()
                .unwrap_or_default(),
        }
    }

    /// The renter pays every fee; the coordinator co-signs only what the
    /// program makes it sign.
    async fn send(
        &self,
        rpc: &Rpc,
        instructions: &[Instruction],
        coordinator_signs: bool,
        timeout: Duration,
    ) -> Result<String, SendError> {
        let blockhash = rpc.latest_blockhash().await.map_err(SendError::Refused)?;
        let mut tx = Transaction::new_with_payer(instructions, Some(&self.renter.pubkey()));
        let signed = if coordinator_signs {
            tx.try_sign(&[&self.renter, &self.coordinator], blockhash)
        } else {
            tx.try_sign(&[&self.renter], blockhash)
        };
        signed.map_err(|e| SendError::Refused(format!("sign: {e}")))?;
        rpc.send_and_confirm(&tx, timeout).await
    }

    async fn send_l1(
        &self,
        instructions: &[Instruction],
        compute_units: u32,
        coordinator_signs: bool,
    ) -> Result<String, SendError> {
        let mut all = vec![
            ComputeBudgetInstruction::set_compute_unit_limit(compute_units),
            ComputeBudgetInstruction::set_compute_unit_price(PRIORITY_MICRO_LAMPORTS),
        ];
        all.extend_from_slice(instructions);
        self.send(&self.l1, &all, coordinator_signs, L1_CONFIRM)
            .await
    }

    /// Rollup transactions are gasless, so they carry no compute budget.
    async fn send_rollup(&self, instructions: &[Instruction]) -> Result<String, SendError> {
        self.send(&self.rollup, instructions, true, ROLLUP_CONFIRM)
            .await
    }
}

fn check_terms(terms: &Terms, parties: &Parties, rate: u64, window: u64) -> Result<(), Failure> {
    let expected = (
        parties.operator,
        parties.coordinator,
        parties.validator,
        parties.mint,
        rate,
        window,
    );
    let found = (
        terms.operator,
        terms.coordinator,
        terms.validator,
        terms.mint,
        terms.rate_per_sec,
        terms.max_duration_secs,
    );
    if expected != found {
        return Err(refused(
            "a lease with different terms is already open for this job",
        ));
    }
    Ok(())
}

fn observation(request: &Request) -> Result<(u64, [u8; 32]), Failure> {
    let metered_ms = request
        .metered_ms
        .ok_or_else(|| refused("metered_ms is required"))?;
    let hash = parse_hex(
        "receipt_hash_hex",
        request.receipt_hash_hex.as_deref().unwrap_or_default(),
    )?;
    Ok((metered_ms, hash))
}

fn parse_pubkey(field: &str, value: &str) -> Result<Pubkey, Failure> {
    Pubkey::from_str(value)
        .map_err(|e| refused(format!("{field} {value:?} is not a public key: {e}")))
}

fn parse_hex<const N: usize>(field: &str, value: &str) -> Result<[u8; N], Failure> {
    let bad = || refused(format!("{field} must be {} hex characters", N * 2));
    if value.len() != N * 2 {
        return Err(bad());
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).map_err(|_| bad())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_ids_are_the_coordinators_simple_uuid_hex() {
        let id: [u8; 16] = parse_hex("job_id", "0123456789abcdef0123456789abcdef").unwrap();
        assert_eq!(id[0], 0x01);
        assert_eq!(id[15], 0xef);
        assert!(parse_hex::<16>("job_id", "0123-4567").is_err());
        assert!(parse_hex::<16>("job_id", "zz23456789abcdef0123456789abcdef").is_err());
    }

    #[test]
    fn a_failure_serializes_the_way_the_coordinator_reads_it() {
        let line = serde_json::to_string(&refused("no lease")).unwrap();
        assert_eq!(line, r#"{"error":"no lease","stage":"not_submitted"}"#);
        let line = serde_json::to_string(&unresolved("lost", Some("sig".into()))).unwrap();
        assert_eq!(
            line,
            r#"{"error":"lost","stage":"maybe_submitted","signature":"sig"}"#
        );
    }

    #[test]
    fn the_same_key_cannot_fund_and_meter() {
        let key = Keypair::new();
        let clone = Keypair::try_from(key.to_bytes().as_slice()).unwrap();
        let request = Request {
            program_id: Pubkey::new_unique().to_string(),
            mint: Pubkey::new_unique().to_string(),
            er_validator: Pubkey::new_unique().to_string(),
            job_id: "00".repeat(16),
            renter: None,
            operator: None,
            rate_micro_usdc_per_sec: None,
            max_duration_secs: None,
            metered_ms: None,
            receipt_hash_hex: None,
            payout_memo: None,
        };
        let refused = Session::new(
            key,
            clone,
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            &request,
        );
        assert!(refused.is_err());
    }
}
