#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env,
    IntoVal, Map, String, Symbol, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum EscrowError {
    JobNotFound = 1,
    Unauthorized = 2,
    InvalidStatus = 3,
    MilestoneNotFound = 4,
    InsufficientFunds = 5,
    AlreadyFunded = 6,
    InvalidDeadline = 7,
    MilestoneDeadlineExceeded = 8,
    HasPendingMilestone = 9,
    NoRefundDue = 10,
    GracePeriodNotMet = 11,
    InvalidMilestoneIndex = 12,
    TokenNotAllowed = 13,
    /// Also returned when an `AddSigner` proposal names an address that is
    /// already a multi-sig signer (issue #1155) — the multi-sig configuration
    /// the proposal asks for is already in place. `EscrowError` is at the SDK's
    /// 50-variant cap, so this reuses the nearest existing variant rather than
    /// adding one, in the same way `create_job` reuses `Unauthorized`.
    AlreadyInitialized = 14,
    ContractPaused = 15,
    NotAdmin = 16,
    /// A revision proposal already exists for this job in Pending status.
    RevisionProposalAlreadyExists = 17,
    /// No revision proposal exists for this job.
    RevisionProposalNotFound = 18,
    /// The caller is not authorized to perform this action on the proposal.
    NotAuthorizedForProposalAction = 19,
    /// The proposal is not in Pending status and cannot be acted upon.
    ProposalNotPending = 20,
    /// Insufficient funds to cover the increased total.
    InsufficientTopUp = 21,
    /// The proposed new_total does not match the sum of milestone amounts.
    ProposalTotalMismatch = 22,
    /// The proposed milestone list is empty.
    EmptyMilestonesProposed = 23,
    /// The job's stored total_amount does not equal the sum of its milestone amounts.
    InvalidAmount = 24,
    /// A milestone is currently in progress or submitted — cancel is not allowed;
    /// the client must open a dispute instead.
    WorkInProgress = 25,
    /// The job deadline has not yet passed; expiry cannot be triggered yet.
    DeadlineNotPassed = 26,
    /// Threshold is 0, exceeds signer count, or a removal would drop below it.
    InvalidThreshold = 27,
    /// The address is not a registered multi-sig signer.
    SignerNotFound = 28,
    /// This signer has already approved the proposal.
    MultiSigAlreadyApproved = 29,
    /// The proposal has already been executed.
    MultiSigAlreadyExecuted = 30,
    /// No multi-sig proposal exists with this ID.
    MultiSigProposalNotFound = 31,
    /// Partial payment amount is invalid (must be > 0 and <= milestone remaining balance).
    InvalidPartialAmount = 32,
    /// The milestone list is empty.
    EmptyMilestones = 33,
    /// The number of milestones exceeds the permitted limit.
    TooManyMilestones = 34,
    /// The fee basis points exceed the maximum permitted limit.
    InvalidFee = 35,
    /// Proposal execution is time-locked and cannot be executed yet.
    ProposalTimeLockActive = 36,
    /// Emergency withdrawal requires the contract to be paused.
    ContractNotPaused = 37,
    /// The job has no escrowed funds available to withdraw.
    NoFundsToWithdraw = 38,
    /// Proposal has expired.
    ProposalExpired = 39,
    /// The proposal TTL has not yet elapsed; it cannot be expired yet.
    ProposalNotExpirable = 40,
    /// The deposited token amount is worth less than the agreed value, beyond the
    /// tolerated slippage. The escrow would under-fund the freelancer.
    InsufficientValue = 41,
    /// The configured price oracle could not be reached or returned invalid data.
    OracleUnavailable = 42,
    /// An arithmetic overflow occurred while computing the deposited value.
    ValueOverflow = 43,
    /// A milestone amount is invalid (zero or negative).
    InvalidMilestone = 44,
    /// Slippage check failed: token value at release time is below the minimum.
    SlippageExceeded = 45,
    /// A replayed nonce was detected within the TTL window.
    NonceReplay = 46,
    /// A milestone deadline is not strictly after the previous milestone's deadline.
    MilestoneDeadlinesNotOrdered = 47,
    /// A milestone deadline is in the past or equal to the current ledger timestamp.
    MilestoneDeadlineInPast = 49,
    /// A revision proposal would drop a milestone that already has disbursed funds,
    /// or would value it below what has already been paid out to the freelancer.
    RevisionBelowDisbursedAmount = 50,

    // ===== Reputation-weighted governance (issue #899) =====
    // The governance proposal lifecycle has its own dedicated `GovError` enum in
    // `governance.rs` (the SDK caps a single error enum at 50 cases). Only this
    // one variant lives here, because it is returned from the multisig code path
    // (`propose_admin_action` / `execute_proposal_internal`) to enforce the
    // governance/multisig separation of powers. Code 51 (not 50) — code 50 is
    // taken by RevisionBelowDisbursedAmount, which landed on main after this
    // branch opened.
    /// A governed parameter must be changed via governance, not the multisig.
    GovernanceRequired = 51,

}

/// Privileged actions that can be proposed and approved through the multi-sig flow.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum AdminAction {
    Pause,
    Unpause,
    SetFeeBps(u32),
    SetTreasury(Address),
    AddSigner(Address),
    RemoveSigner(Address),
    ChangeThreshold(u32),
    RotateSigner(Address, Address),
    /// Emergency withdrawal: recover escrowed funds from a specific job to a recipient address.
    /// Only executable when the contract is paused. Requires multi-sig approval.
    EmergencyWithdraw(u64, Address),
}

/// A pending multi-sig proposal. Executed when `approvals.len() >= threshold`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MultiSigProposal {
    pub id: u64,
    pub action: AdminAction,
    pub proposer: Address,
    pub approvals: Vec<Address>,
    pub executed: bool,
    pub created_at: u64,
}

/// # Escrow State Machine
///
/// The escrow contract enforces a strict state machine to ensure valid transitions
/// and prevent invalid operations. Each state has specific allowed transitions:
///
/// ```text
/// ┌─────────┐
/// │ Created │ ──fund_job──> ┌────────┐
/// └─────────┘               │ Funded │
///                           └────────┘
///                                │
///                                │ submit_milestone
///                                ▼
///                          ┌────────────┐
///                          │ InProgress │
///                          └────────────┘
///                                │
///                    ┌───────────┼───────────┐
///                    │           │           │
///         approve_milestone   dispute    expire_job
///                    │           │           │
///                    ▼           ▼           ▼
///              ┌───────────┐ ┌──────────┐ ┌─────────┐
///              │ Completed │ │ Disputed │ │ Expired │
///              └───────────┘ └──────────┘ └─────────┘
///                                │
///                    resolve_dispute_callback
///                                │
///                    ┌───────────┼───────────────────────┐
///                    │           │                       │
///          FreelancerWins    ClientWins /            Escalate
///                    │       RefundBoth /                 │
///                    │       RefundSplit /       (status unchanged —
///                    │       MaliciousFiling      job stays Disputed,
///                    │           │                no funds moved)
///                    ▼           ▼                        │
///              ┌───────────┐ ┌───────────┐                │
///              │ Completed │ │ Cancelled │ <──────────────┘
///              └───────────┘ └───────────┘   a later resolve_dispute_callback
///                    ▲                       with a final resolution
///                    │
///                    └── (or expire_job once the deadline passes ──> Expired)
/// ```
///
/// ## State Descriptions
///
/// - **Created**: Job has been created but not yet funded. Only `fund_job` is allowed.
/// - **Funded**: Escrow is funded. Freelancer can start work via `submit_milestone`.
/// - **InProgress**: Work has begun. Milestones can be submitted, approved, or disputed.
/// - **Completed**: All milestones approved and payments released. Terminal state.
/// - **Disputed**: A dispute has been raised. Only dispute resolution can change state.
///   Not a terminal state: an `Escalate` resolution leaves the job here (see
///   [Escalated disputes](#escalated-disputes)) until a later resolution or
///   `expire_job` moves it on.
/// - **Cancelled**: Job was cancelled or refunded. Terminal state.
/// - **Expired**: Job deadline passed without completion. Terminal state.
///
/// ## Transition Rules
///
/// | From State  | To State    | Trigger Function              | Conditions                          |
/// |-------------|-------------|-------------------------------|-------------------------------------|
/// | Created     | Funded      | `fund_job`                    | Client transfers full amount        |
/// | Funded      | InProgress  | `submit_milestone`            | Freelancer submits first milestone  |
/// | Funded      | Cancelled   | `cancel_job`                  | No work started, client cancels     |
/// | Funded      | Expired     | `expire_job`                  | Deadline passed                     |
/// | InProgress  | Completed   | `approve_milestone`           | All milestones approved             |
/// | InProgress  | Disputed    | External dispute contract     | Either party raises dispute         |
/// | InProgress  | Cancelled   | `cancel_job`                  | No active work, client cancels      |
/// | InProgress  | Expired     | `expire_job`                  | Deadline passed                     |
/// | Disputed    | Completed   | `resolve_dispute_callback`    | `FreelancerWins`                    |
/// | Disputed    | Cancelled   | `resolve_dispute_callback`    | `ClientWins`, `RefundBoth`, `RefundSplit`, `MaliciousFiling` |
/// | Disputed    | Disputed    | `resolve_dispute_callback`    | `Escalate` — no state change, no payout |
/// | Disputed    | Expired     | `expire_job`                  | Deadline passed while still disputed |
///
/// Terminal states (Completed, Cancelled, Expired) cannot transition to any other state.
///
/// ## Escalated disputes
///
/// [`DisputeResolution::Escalate`] is the one resolution that does **not** settle a
/// job. In `resolve_dispute_callback` its arm is deliberately empty: no token is
/// transferred (`apply_dispute_distribution` is a no-op for `Escalate`,
/// and `calculate_payout` reports an all-zero [`PayoutBreakdown`]) and the job's
/// status is left exactly as it was — a disputed job stays `Disputed`, holding the
/// full escrowed balance. It is a self-loop, not a terminal transition.
///
/// The escalation itself is recorded off this contract: `resolve_dispute_callback`
/// still emits the `("escrow", "dispute")` event carrying `Escalate`, which is the
/// signal for the dispute contract / off-chain arbitration tier to pick the case up.
/// This contract holds no escalation queue, deadline, or auto-resolution timer of
/// its own, so nothing here will move the job on by itself.
///
/// **How an escalated job leaves `Disputed`** — one of two paths, both externally
/// driven:
///
/// 1. **Re-resolution (expected path).** The dispute contract calls
///    `resolve_dispute_callback` again with a final resolution. This is permitted
///    because `require_state_disputable` accepts `Disputed` as an input state, so
///    an escalated job can be resolved any number of times until a non-`Escalate`
///    resolution settles it. The remaining balance is recomputed from the job's
///    approved milestones at that point, so no funds are lost by escalating first.
/// 2. **Expiry (backstop).** Once `job.job_deadline` has passed, anyone may call
///    `expire_job`; `require_state_expirable` rejects only the terminal states, so a
///    `Disputed` job is expirable. Approved milestones are paid out to the freelancer
///    and the remainder is refunded to the client, and the job becomes `Expired`.
///
/// Responsibility for driving path 1 sits with the dispute contract's arbitration
/// tier, not with the escrow contract. If it never does so, the job remains in
/// `Disputed` with funds escrowed until the deadline makes path 2 available.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JobStatus {
    Created,
    Funded,
    InProgress,
    Completed,
    Disputed,
    Cancelled,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisputeResolution {
    ClientWins,
    FreelancerWins,
    RefundBoth,
    RefundSplit(u32),
    /// Hand the dispute to a higher arbitration tier. Settles nothing: no funds are
    /// moved and the job keeps its current status (a disputed job stays `Disputed`).
    /// See the [`JobStatus`] state-machine docs, "Escalated disputes".
    Escalate,
    /// Dispute was filed in bad faith; initiator's full stake is sent to treasury.
    MaliciousFiling,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PayoutBreakdown {
    pub client: i128,
    pub freelancer: i128,
    pub platform: i128,
    pub arbitrators: i128,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MilestoneStatus {
    Pending,
    InProgress,
    Submitted,
    Approved,
    /// Milestone has been partially paid; `amount` now holds the REMAINING unpaid balance.
    PartiallyPaid,
}

/// Represents the lifecycle state of a revision proposal.
/// A proposal begins as Pending and transitions to either Accepted or Rejected.
/// Only one transition is permitted — a resolved proposal cannot be re-opened.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum ProposalStatus {
    /// The proposal has been submitted and is awaiting a response from the opposing party.
    Pending,
    /// The opposing party has accepted the proposal. Job milestones and escrow have been updated.
    Accepted,
    /// The opposing party has rejected the proposal. No changes were made to the job.
    Rejected,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    pub id: u32,
    pub description: String,
    /// For a `PartiallyPaid` milestone this is the REMAINING unpaid balance.
    pub amount: i128,
    pub status: MilestoneStatus,
    pub deadline: u64,
    /// Payment token for this milestone. `None` falls back to `Job.token`.
    pub token: Option<Address>,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Job {
    pub id: u64,
    pub client: Address,
    pub freelancer: Address,
    /// Default payment token. Milestones with `token: None` use this.
    pub token: Address,
    /// Sum of all milestone amounts denominated in `token` (the default token).
    pub total_amount: i128,
    /// Default-token amount actually deposited into this contract for this job.
    /// Starts at 0, set by `fund_job`, updated by `top_up_escrow` and `accept_revision`.
    pub funded_amount: i128,
    pub status: JobStatus,
    pub milestones: Vec<Milestone>,
    pub job_deadline: u64,
    pub auto_refund_after: u64,
    /// Ledger number at which the escrow expires and funds can be auto-released.
    pub expiry_ledger: u32,
    /// Per-non-default-token funding balances for multi-token jobs.
    /// Empty for single-token jobs (all milestones use `Job.token`).
    pub token_balances: Vec<TokenBalance>,
}

pub(crate) const MAX_FEE_BPS: u32 = 500; // 5%
const MAX_MILESTONES: u32 = 50;

/// A formal proposal to revise the milestones and total budget of an active job.
#[contracttype]
#[derive(Clone, Debug)]
pub struct RevisionProposal {
    pub proposer: Address,
    pub new_milestones: Vec<Milestone>,
    pub new_total: i128,
    pub status: ProposalStatus,
    pub created_at: u64,
}

/// Per-token funding balance for multi-token jobs.
///
/// For jobs where different milestones use different tokens, this struct
/// tracks how much of each non-default token is required and how much
/// has actually been deposited.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenBalance {
    pub token: Address,
    /// Sum of all milestone amounts denominated in this token.
    pub total_amount: i128,
    /// Tokens actually deposited into escrow for this token.
    pub funded_amount: i128,
}

/// A snapshot of the exchange-rate parity check performed at funding time.
///
/// `twap_price` is the time-weighted average price of the funding token quoted in
/// XLM stroops, scaled by [`PRICE_SCALE`]. `deposited_value` is the resulting
/// value of the deposit in XLM stroops. Stored per job for audit / UI display.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateSnapshot {
    /// TWAP of `token` quoted in XLM stroops, scaled by `PRICE_SCALE`.
    pub twap_price: i128,
    /// Number of ledger samples the oracle averaged over.
    pub samples: u32,
    /// Agreed job value in XLM stroops (0 when oracle validation was bypassed).
    pub agreed_value_stroops: i128,
    /// Computed value of the deposit in XLM stroops at funding time.
    pub deposited_value: i128,
    /// Tolerated downside deviation in basis points.
    pub max_slippage_bps: u32,
    /// Ledger sequence at which the snapshot was taken.
    pub ledger: u32,
}

/// A snapshot of milestones at a specific point in time for audit trail purposes.
#[contracttype]
#[derive(Clone, Debug)]
pub struct MilestoneRevision {
    pub revision_index: u32,
    pub milestones: Vec<Milestone>,
    pub total_amount: i128,
    pub revised_at: u64,
    pub revised_by: Address,
}

/// Status of a freelancer-to-sub-freelancer milestone sub-assignment.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubAssignmentStatus {
    /// Sub-assignment is live; payout will be split at milestone approval.
    Active,
    /// Sub-freelancer has been paid when the milestone was approved.
    Paid,
    /// Assignment was cancelled before the milestone was approved.
    Cancelled,
}

/// Records a freelancer's promise to pay a sub-freelancer a fixed amount
/// from the milestone payout when the client approves that milestone.
/// No funds are pre-escrowed; the split is enforced at payout time.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SubAssignment {
    pub job_id: u64,
    pub milestone_id: u32,
    pub sub_freelancer: Address,
    /// Token amount reserved for the sub-freelancer (in the milestone's token).
    pub amount: i128,
    pub status: SubAssignmentStatus,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataKey {
    Job(u64),
    JobCount,
    Admin, // Legacy single admin
    Paused,
    AllowedTokens,
    RevisionProposal(u64),
    ProposalExpiry,
    MultiSigSigners,   // Vec<Address>
    MultiSigThreshold, // u32
    MultiSigProposal(u64),
    MultiSigProposalCount,
    MultiSigExecutionNotBefore(u64),
    RevisionHistory(u64), // Vec<MilestoneRevision> keyed by job_id
    MilestoneSubmittedAt(u64, u32),
    InactivityAutoApproveAt(u64, u32),
    /// Address of the price oracle contract used for exchange-rate parity checks.
    PriceOracle,
    /// Stored RateSnapshot from the parity check performed at funding time.
    RateSnapshot(u64),
    /// Per-caller nonce to prevent replay attacks within the TTL window.
    Nonce(Address, Symbol, u64),
    /// Registered dispute contract — the only address allowed to call mark_job_disputed.
    DisputeContract,
    /// Cumulative amount (i128) already paid out to the freelancer for a given
    /// (job_id, milestone_id), via `release_partial_payment` or `release_milestone`.
    /// Used by `accept_revision` to ensure a revision never shrinks a milestone's
    /// value below funds that have already left escrow for it.
    MilestoneDisbursed(u64, u32),
    /// Sub-assignment created by the main freelancer for a specific milestone.
    /// Keyed by (job_id, milestone_id). At most one sub-assignment per milestone.
    SubAssignment(u64, u32),
}

/// Fixed-point scale for oracle prices: prices are quoted in XLM stroops per token
/// unit, multiplied by this factor (1e7, matching Stellar's 7-decimal precision).
const PRICE_SCALE: i128 = 10_000_000;

/// Minimum number of ledger samples the TWAP must be averaged over.
const MIN_TWAP_SAMPLES: u32 = 10;

/// Number of ledger samples requested from the oracle.
const TWAP_SAMPLE_LEDGERS: u32 = 10;

/// Default proposal expiry: 7 days in seconds.
const DEFAULT_PROPOSAL_EXPIRY_SECS: u64 = 7 * 24 * 3600;
const INACTIVITY_THRESHOLD_SECS: u64 = 7 * 24 * 3600;
const INACTIVITY_GRACE_SECS: u64 = 3 * 24 * 3600;
const MULTISIG_TIME_LOCK_SECS: u64 = 48 * 60 * 60;
const PROPOSAL_TTL: u64 = 7 * 24 * 60 * 60;

const NONCE_EXPIRY_LEDGERS: u32 = 3;

fn get_job_key(job_id: u64) -> DataKey {
    DataKey::Job(job_id)
}

/// Returns the payment token for a milestone, falling back to the job-level default.
fn resolve_milestone_token(milestone: &Milestone, job: &Job) -> Address {
    milestone.token.clone().unwrap_or_else(|| job.token.clone())
}

/// Transfer `amount` of `token` from this contract to the appropriate party according
/// to the dispute resolution. No-op when amount <= 0 or resolution is Escalate.
fn apply_dispute_distribution(
    env: &Env,
    token_addr: &Address,
    amount: i128,
    resolution: &DisputeResolution,
    client: &Address,
    freelancer: &Address,
    treasury: &Address,
) {
    if amount <= 0 {
        return;
    }
    let tc = token::Client::new(env, token_addr);
    let contract = env.current_contract_address();
    match resolution {
        DisputeResolution::ClientWins => {
            tc.transfer(&contract, client, &amount);
        }
        DisputeResolution::FreelancerWins => {
            tc.transfer(&contract, freelancer, &amount);
        }
        DisputeResolution::RefundBoth => {
            let half = amount / 2;
            if half > 0 {
                tc.transfer(&contract, client, &half);
                tc.transfer(&contract, freelancer, &(amount - half));
            }
        }
        DisputeResolution::RefundSplit(pct_client) => {
            let pct = if *pct_client > 100 { 100 } else { *pct_client } as i128;
            let ca = (amount * pct) / 100;
            let fa = amount - ca;
            if ca > 0 {
                tc.transfer(&contract, client, &ca);
            }
            if fa > 0 {
                tc.transfer(&contract, freelancer, &fa);
            }
        }
        DisputeResolution::MaliciousFiling => {
            tc.transfer(&contract, treasury, &amount);
        }
        DisputeResolution::Escalate => {}
    }
}

/// Cumulative amount already disbursed to the freelancer for a given milestone,
/// across `release_partial_payment` and `release_milestone` calls.
fn get_milestone_disbursed(env: &Env, job_id: u64, milestone_id: u32) -> i128 {
    env.storage()
        .persistent()
        .get(&DataKey::MilestoneDisbursed(job_id, milestone_id))
        .unwrap_or(0)
}

/// Records that `amount` additional tokens have just been paid out to the freelancer
/// for `milestone_id`, so later revisions can't retroactively undercut it.
fn record_milestone_disbursed(env: &Env, job_id: u64, milestone_id: u32, amount: i128) {
    let key = DataKey::MilestoneDisbursed(job_id, milestone_id);
    let existing: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    env.storage().persistent().set(&key, &existing.saturating_add(amount));
    env.storage()
        .persistent()
        .extend_ttl(&key, TTL_THRESHOLD_LEDGERS, TTL_EXTEND_TO_LEDGERS);
}

fn consume_nonce(env: &Env, caller: &Address, function: &Symbol, nonce: u64) -> Result<(), EscrowError> {
    let key = DataKey::Nonce(caller.clone(), function.clone(), nonce);
    if env.storage().temporary().has(&key) {
        return Err(EscrowError::NonceReplay);
    }
    env.storage().temporary().set(&key, &true);
    env.storage().temporary().extend_ttl(&key, NONCE_EXPIRY_LEDGERS, NONCE_EXPIRY_LEDGERS);
    Ok(())
}

fn require_not_paused(env: &Env) -> Result<(), EscrowError> {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(EscrowError::ContractPaused);
    }
    Ok(())
}

/// Validates that every address in `callers` is a registered signer.
fn is_signer(env: &Env, address: &Address) -> bool {
    if let Some(signers) = env
        .storage()
        .instance()
        .get::<_, Vec<Address>>(&DataKey::MultiSigSigners)
    {
        signers.iter().any(|s| s == *address)
    } else {
        false
    }
}

/// Loads a multi-sig proposal from wherever it currently lives (issue #1153).
///
/// Pending proposals sit in instance storage so the governance flow can read and
/// mutate them cheaply. Terminal ones (executed or expired) have been moved to
/// persistent storage by `archive_proposal`. Looking in both places keeps error
/// reporting honest: re-approving an already-executed proposal still reports
/// `MultiSigAlreadyExecuted` rather than degrading to "not found".
fn load_proposal(env: &Env, proposal_id: u64) -> Option<MultiSigProposal> {
    let key = DataKey::MultiSigProposal(proposal_id);
    env.storage()
        .instance()
        .get(&key)
        .or_else(|| env.storage().persistent().get(&key))
}

/// Moves a terminal proposal out of instance storage and into persistent storage
/// under a bounded TTL (issue #1153).
///
/// Instance storage is read in full on every invocation of this contract, so a
/// proposal that can never be acted on again must not stay there. The companion
/// `MultiSigExecutionNotBefore` entry is dropped outright — it only gates
/// execution, which is no longer possible.
fn archive_proposal(env: &Env, proposal_id: u64, proposal: &MultiSigProposal) {
    let key = DataKey::MultiSigProposal(proposal_id);

    env.storage().instance().remove(&key);
    env.storage()
        .instance()
        .remove(&DataKey::MultiSigExecutionNotBefore(proposal_id));

    env.storage().persistent().set(&key, proposal);
    env.storage().persistent().extend_ttl(
        &key,
        PROPOSAL_ARCHIVE_TTL_THRESHOLD,
        PROPOSAL_ARCHIVE_TTL_LEDGERS,
    );
}

// Production TTL constants based on Stellar's ~5-second ledger close time
const LEDGERS_PER_DAY: u32 = 17_280; // 86,400 seconds/day ÷ 5 seconds/ledger
const TTL_THRESHOLD_LEDGERS: u32 = LEDGERS_PER_DAY * 15; // 15 days = 259,200 ledgers
const TTL_EXTEND_TO_LEDGERS: u32 = LEDGERS_PER_DAY * 30; // 30 days = 518,400 ledgers
// Instance storage needs a much larger TTL so multi-period tests don't archive it.
const INSTANCE_TTL_THRESHOLD: u32 = 50_000_000;
const INSTANCE_TTL_EXTEND_TO: u32 = 50_000_000;

const ESCROW_TTL_LEDGERS: u32 = 535_000; // ~90 days at 5s/ledger

// Bounded TTL for archived multi-sig proposals (issue #1153). Once a proposal
// reaches a terminal state (executed or expired) it is moved out of instance
// storage — which is loaded in full on *every* contract invocation — into
// persistent storage under the same `DataKey::MultiSigProposal(id)` key. The
// record stays readable by auditors and off-chain tooling for ~30 days and then
// expires on its own, so governance history can never grow without bound.
const PROPOSAL_ARCHIVE_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 15; // 15 days
const PROPOSAL_ARCHIVE_TTL_LEDGERS: u32 = LEDGERS_PER_DAY * 30; // 30 days

type EscrowKey = DataKey;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TtlExtendedEvent {
    pub job_id: u64,
    pub new_expiry_ledger: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paused {
    pub paused_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unpaused {
    pub unpaused_by: Address,
}


fn bump_escrow_ttl(env: &Env, job_id: u64) {
    let key = EscrowKey::Job(job_id);
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, ESCROW_TTL_LEDGERS, ESCROW_TTL_LEDGERS);
    }
}

fn bump_job_ttl(env: &Env, job_id: u64) {
    bump_escrow_ttl(env, job_id);
}

fn bump_job_count_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

// ============================================================
// STATE MACHINE VALIDATION
// ============================================================
// These functions enforce the escrow state machine by validating
// that state transitions are legal before any mutation occurs.
// Each function returns EscrowError::InvalidState if the current
// state does not permit the requested operation.
// ============================================================

/// Validates that the job is in Created state (ready to be funded).
fn require_state_created(job: &Job) -> Result<(), EscrowError> {
    if job.status != JobStatus::Created {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is in Funded or InProgress state (work can proceed).
fn require_state_funded_or_in_progress(job: &Job) -> Result<(), EscrowError> {
    if job.status != JobStatus::Funded && job.status != JobStatus::InProgress {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is NOT in a terminal state (Completed, Cancelled, Expired).
/// Terminal states cannot transition to any other state.
fn require_state_not_terminal(job: &Job) -> Result<(), EscrowError> {
    if job.status == JobStatus::Completed
        || job.status == JobStatus::Cancelled
        || job.status == JobStatus::Expired
    {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is NOT disputed.
/// Most operations are blocked during active disputes.
fn require_state_not_disputed(job: &Job) -> Result<(), EscrowError> {
    if job.status == JobStatus::Disputed {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is in a state that can be disputed.
/// Only Funded, InProgress, or already Disputed jobs can have dispute operations.
fn require_state_disputable(job: &Job) -> Result<(), EscrowError> {
    if job.status != JobStatus::Funded
        && job.status != JobStatus::InProgress
        && job.status != JobStatus::Disputed
    {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is in a state that can be cancelled.
/// Only Funded or InProgress jobs can be cancelled (and only if no work is in progress).
fn require_state_cancellable(job: &Job) -> Result<(), EscrowError> {
    if job.status != JobStatus::Funded && job.status != JobStatus::InProgress {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Validates that the job is in a state that can expire.
/// Jobs in Created, Completed, Cancelled, or Expired states cannot expire.
fn require_state_expirable(job: &Job) -> Result<(), EscrowError> {
    if job.status == JobStatus::Completed
        || job.status == JobStatus::Cancelled
        || job.status == JobStatus::Expired
    {
        return Err(EscrowError::InvalidStatus);
    }
    Ok(())
}

/// Query the configured price oracle for the TWAP of `token` quoted in XLM
/// stroops (scaled by [`PRICE_SCALE`]) over the last [`TWAP_SAMPLE_LEDGERS`]
/// ledgers.
///
/// The oracle is expected to expose:
/// `twap(token: Address, quote: Address, sample_ledgers: u32) -> (i128 price, u32 samples)`
///
/// Returns `OracleUnavailable` if no oracle is configured, the cross-contract
/// call traps, or the oracle reports fewer than [`MIN_TWAP_SAMPLES`] samples or a
/// non-positive price.
fn fetch_twap_price(env: &Env, token: &Address) -> Result<(i128, u32), EscrowError> {
    let oracle: Address = env
        .storage()
        .instance()
        .get(&DataKey::PriceOracle)
        .ok_or(EscrowError::OracleUnavailable)?;

    // The native XLM SAC address is the quote asset (price denominated in XLM).
    let quote = env.current_contract_address();

    let args = soroban_sdk::vec![
        env,
        token.clone().into_val(env),
        quote.into_val(env),
        TWAP_SAMPLE_LEDGERS.into_val(env),
    ];

    // `try_invoke_contract` surfaces a host error as Err rather than trapping,
    // so an unavailable oracle becomes OracleUnavailable instead of a panic.
    let result = env.try_invoke_contract::<(i128, u32), soroban_sdk::Error>(
        &oracle,
        &Symbol::new(env, "twap"),
        args,
    );

    let (price, samples) = match result {
        Ok(Ok(value)) => value,
        _ => return Err(EscrowError::OracleUnavailable),
    };

    if price <= 0 || samples < MIN_TWAP_SAMPLES {
        return Err(EscrowError::OracleUnavailable);
    }

    Ok((price, samples))
}

/// Compute `amount * twap_price / PRICE_SCALE` in XLM stroops with overflow-safe
/// arithmetic. Returns [`EscrowError::ValueOverflow`] instead of wrapping or
/// panicking for any `amount`/`twap_price` pair.
fn compute_deposited_value(amount: i128, twap_price: i128) -> Result<i128, EscrowError> {
    amount
        .checked_mul(twap_price)
        .ok_or(EscrowError::ValueOverflow)?
        .checked_div(PRICE_SCALE)
        .ok_or(EscrowError::ValueOverflow)
}

/// Validate that depositing `amount` of `token` is worth at least
/// `agreed_value_stroops`, within `max_slippage_bps` downside tolerance, using a
/// TWAP from the oracle. Returns the audit [`RateSnapshot`] on success.
///
/// When `agreed_value_stroops == 0` the check is bypassed (e.g. native XLM jobs)
/// and an empty snapshot is returned without contacting the oracle.
fn validate_deposit_value(
    env: &Env,
    token: &Address,
    amount: i128,
    agreed_value_stroops: i128,
    max_slippage_bps: u32,
) -> Result<RateSnapshot, EscrowError> {
    if agreed_value_stroops == 0 {
        return Ok(RateSnapshot {
            twap_price: 0,
            samples: 0,
            agreed_value_stroops: 0,
            deposited_value: 0,
            max_slippage_bps,
            ledger: env.ledger().sequence(),
        });
    }

    let (twap_price, samples) = fetch_twap_price(env, token)?;

    // deposited_value = amount * twap_price / PRICE_SCALE, with overflow checks
    // so adversarial amount/price values can never wrap i128.
    let deposited_value = compute_deposited_value(amount, twap_price)?;

    // Minimum acceptable value after applying slippage tolerance:
    // agreed * (10000 - slippage_bps) / 10000.
    let bps_factor = 10_000i128
        .checked_sub(max_slippage_bps as i128)
        .ok_or(EscrowError::ValueOverflow)?;
    let min_value = agreed_value_stroops
        .checked_mul(bps_factor)
        .ok_or(EscrowError::ValueOverflow)?
        .checked_div(10_000)
        .ok_or(EscrowError::ValueOverflow)?;

    if deposited_value < min_value {
        return Err(EscrowError::InsufficientValue);
    }

    Ok(RateSnapshot {
        twap_price,
        samples,
        agreed_value_stroops,
        deposited_value,
        max_slippage_bps,
        ledger: env.ledger().sequence(),
    })
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Initialize the contract with signers, threshold, treasury, fee basis points, and proposal expiry.
    pub fn initialize(
        env: Env,
        signers: Vec<Address>,
        threshold: u32,
        treasury: Address,
        fee_bps: u32,
        proposal_expiry_secs: u64,
    ) -> Result<(), EscrowError> {
        if env.storage().instance().has(&DataKey::MultiSigSigners) {
            return Err(EscrowError::AlreadyInitialized);
        }
        if fee_bps > MAX_FEE_BPS {
            return Err(EscrowError::InvalidFee);
        }
        if threshold == 0 || threshold > signers.len() {
            return Err(EscrowError::InvalidThreshold);
        }

        env.storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &signers);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &threshold);

        env.storage()
            .instance()
            .set(&symbol_short!("TRE"), &treasury);
        env.storage()
            .instance()
            .set(&symbol_short!("FEE"), &fee_bps);
        env.storage().instance().set(&DataKey::Paused, &false);
        let allowed_tokens: Vec<Address> = Vec::new(&env);
        env.storage()
            .instance()
            .set(&DataKey::AllowedTokens, &allowed_tokens);
        env.storage()
            .instance()
            .set(&DataKey::ProposalExpiry, &proposal_expiry_secs);
        bump_job_count_ttl(&env);

        Ok(())
    }

    pub fn add_allowed_token(env: Env, admin: Address, token: Address) -> Result<(), EscrowError> {
        require_not_paused(&env)?;

        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(EscrowError::NotAdmin);
        }

        let mut allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or(Vec::new(&env));
        if !allowed.iter().any(|t| t == token) {
            allowed.push_back(token.clone());
            env.storage()
                .instance()
                .set(&DataKey::AllowedTokens, &allowed);
            env.events().publish(
                (symbol_short!("escrow"), Symbol::new(&env, "token_allowed")),
                (token.clone(), admin.clone()),
            );
        }
        Ok(())
    }

    pub fn remove_allowed_token(
        env: Env,
        admin: Address,
        token: Address,
    ) -> Result<(), EscrowError> {
        require_not_paused(&env)?;

        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(EscrowError::NotAdmin);
        }

        let mut allowed: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or(Vec::new(&env));
        if let Some(index) = allowed.iter().position(|t| t == token) {
            allowed.remove(index as u32);
            env.storage()
                .instance()
                .set(&DataKey::AllowedTokens, &allowed);
            env.events().publish(
                (symbol_short!("escrow"), Symbol::new(&env, "token_revoked")),
                (token.clone(), admin.clone()),
            );
        }
        Ok(())
    }

    pub fn get_allowed_tokens(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::AllowedTokens)
            .unwrap_or(Vec::new(&env))
    }

    /// Check if the contract is paused.
    pub fn is_paused(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    /// Configure the price oracle contract used for exchange-rate parity checks.
    /// Admin (registered signer) only. The oracle must expose
    /// `twap(token: Address, quote: Address, sample_ledgers: u32) -> (i128, u32)`.
    pub fn set_price_oracle(
        env: Env,
        admin: Address,
        oracle: Address,
    ) -> Result<(), EscrowError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(EscrowError::NotAdmin);
        }
        env.storage().instance().set(&DataKey::PriceOracle, &oracle);
        Ok(())
    }

    /// Return the configured price oracle address, if any.
    pub fn get_price_oracle(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PriceOracle)
    }

    /// Register the dispute contract address. Only a registered multisig signer may call this.
    pub fn set_dispute_contract(
        env: Env,
        admin: Address,
        dispute_contract: Address,
    ) -> Result<(), EscrowError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(EscrowError::NotAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::DisputeContract, &dispute_contract);
        Ok(())
    }

    /// Called by the registered dispute contract to transition a job to the Disputed state
    /// and emit a structured DisputeRaised event that indexers can consume.
    pub fn mark_job_disputed(
        env: Env,
        job_id: u64,
        dispute_id: u64,
    ) -> Result<(), EscrowError> {
        let dispute_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::DisputeContract)
            .ok_or(EscrowError::Unauthorized)?;
        dispute_contract.require_auth();

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        require_state_disputable(&job)?;

        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&get_job_key(job_id), &job);

        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "disputed")),
            (job_id, dispute_id, job.client, job.freelancer),
        );

        Ok(())
    }

    /// Return the exchange-rate parity snapshot recorded when the job was funded.
    /// `None` for jobs funded without oracle validation (legacy / XLM-only).
    pub fn get_rate_snapshot(env: Env, job_id: u64) -> Option<RateSnapshot> {
        bump_escrow_ttl(&env, job_id);
        env.storage()
            .persistent()
            .get(&DataKey::RateSnapshot(job_id))
    }

    pub fn propose_admin_action(
        env: Env,
        proposer: Address,
        action: AdminAction,
    ) -> Result<u64, EscrowError> {
        proposer.require_auth();
        if !is_signer(&env, &proposer) {
            return Err(EscrowError::SignerNotFound);
        }

        // Reject governed parameter changes up front once governance is enabled,
        // so the multisig cannot even queue a proposal that governance owns.
        // Execution is guarded independently in `execute_proposal_internal`.
        if governance::governance_enabled(&env) && governance::is_governable_action(&action) {
            return Err(EscrowError::GovernanceRequired);
        }

        // Validate the action's payload before the proposal is stored and opened
        // for approval (issue #1154). `execute_proposal_internal` re-checks this,
        // but catching it here means signers never spend coordination effort
        // approving a proposal that was invalid the moment it was created.
        if let AdminAction::SetFeeBps(fee) = action {
            if fee > MAX_FEE_BPS {
                return Err(EscrowError::InvalidFee);
            }
        }

        let mut count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigProposalCount)
            .unwrap_or(0);
        count += 1;

        let mut approvals = Vec::new(&env);
        approvals.push_back(proposer.clone());

        let now = env.ledger().timestamp();
        let execution_not_before = match action {
            AdminAction::Pause | AdminAction::SetTreasury(_) => {
                now.saturating_add(MULTISIG_TIME_LOCK_SECS)
            }
            _ => now,
        };

        let proposal = MultiSigProposal {
            id: count,
            action: action.clone(),
            proposer: proposer.clone(),
            approvals,
            executed: false,
            created_at: now,
        };

        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposal(count), &proposal);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposalCount, &count);
        env.storage().instance().set(
            &DataKey::MultiSigExecutionNotBefore(count),
            &execution_not_before,
        );

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("proposed")),
            (count, proposer, action),
        );

        // Auto-execute if threshold is 1 and time-lock not active
        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .unwrap_or(1);
        if threshold == 1 && now >= execution_not_before {
            Self::execute_proposal_internal(&env, count)?;
        }

        Ok(count)
    }

    pub fn approve_admin_action(
        env: Env,
        approver: Address,
        proposal_id: u64,
    ) -> Result<(), EscrowError> {
        approver.require_auth();
        if !is_signer(&env, &approver) {
            return Err(EscrowError::SignerNotFound);
        }

        let mut proposal: MultiSigProposal =
            load_proposal(&env, proposal_id).ok_or(EscrowError::MultiSigProposalNotFound)?;

        if proposal.executed {
            return Err(EscrowError::MultiSigAlreadyExecuted);
        }

        if env.ledger().timestamp() > proposal.created_at + PROPOSAL_TTL {
            return Err(EscrowError::ProposalExpired);
        }

        if proposal.approvals.iter().any(|a| a == approver) {
            return Err(EscrowError::MultiSigAlreadyApproved);
        }

        proposal.approvals.push_back(approver.clone());
        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposal(proposal_id), &proposal);

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("approved")),
            (proposal_id, approver),
        );

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .unwrap_or(1);
        if proposal.approvals.len() >= threshold {
            let not_before: u64 = env
                .storage()
                .instance()
                .get(&DataKey::MultiSigExecutionNotBefore(proposal_id))
                .unwrap_or(proposal.created_at);
            if env.ledger().timestamp() >= not_before {
                Self::execute_proposal_internal(&env, proposal_id)?;
            }
        }

        Ok(())
    }

    pub fn execute_proposal(
        env: Env,
        caller: Address,
        proposal_id: u64,
    ) -> Result<(), EscrowError> {
        caller.require_auth();
        if !is_signer(&env, &caller) {
            return Err(EscrowError::SignerNotFound);
        }
        Self::execute_proposal_internal(&env, proposal_id)
    }

    /// Retires an expired multi-sig proposal from instance storage (issue #1153).
    ///
    /// Executed proposals are archived automatically by `execute_proposal_internal`,
    /// but a proposal that never reached its approval threshold simply goes stale
    /// and would otherwise sit in instance storage forever. Instance storage is
    /// loaded in full on every contract invocation, so stale governance proposals
    /// tax every unrelated call. This moves such a proposal to persistent storage
    /// under a bounded TTL, leaving instance storage holding only genuinely
    /// pending proposals.
    ///
    /// # Authorization
    /// Permissionless, in the same spirit as `bump_escrow`: it is pure storage
    /// maintenance. Only proposals that are already past `PROPOSAL_TTL` — and so
    /// can no longer be approved or executed — are eligible, meaning a caller
    /// cannot use this to interfere with live governance.
    ///
    /// # Errors
    /// * `MultiSigProposalNotFound` — no pending proposal with this ID
    /// * `ProposalNotExpirable`     — the proposal's TTL has not yet elapsed
    pub fn prune_expired_proposal(env: Env, proposal_id: u64) -> Result<(), EscrowError> {
        let proposal: MultiSigProposal = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigProposal(proposal_id))
            .ok_or(EscrowError::MultiSigProposalNotFound)?;

        if env.ledger().timestamp() <= proposal.created_at + PROPOSAL_TTL {
            return Err(EscrowError::ProposalNotExpirable);
        }

        archive_proposal(&env, proposal_id, &proposal);

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("pruned")),
            (proposal_id, proposal.proposer),
        );

        Ok(())
    }

    fn execute_proposal_internal(env: &Env, proposal_id: u64) -> Result<(), EscrowError> {
        let mut proposal: MultiSigProposal =
            load_proposal(env, proposal_id).ok_or(EscrowError::MultiSigProposalNotFound)?;

        if proposal.executed {
            return Err(EscrowError::MultiSigAlreadyExecuted);
        }

        if env.ledger().timestamp() > proposal.created_at + PROPOSAL_TTL {
            return Err(EscrowError::ProposalExpired);
        }

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .unwrap_or(1);
        if proposal.approvals.len() < threshold {
            return Err(EscrowError::Unauthorized);
        }

        let not_before: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigExecutionNotBefore(proposal_id))
            .unwrap_or(proposal.created_at);
        if env.ledger().timestamp() < not_before {
            return Err(EscrowError::ProposalTimeLockActive);
        }

        // Governance/multisig separation of powers (issue #899): once governance
        // is configured, protocol-parameter changes (fee, treasury) are the
        // exclusive domain of the reputation-weighted governance path. The
        // multisig retains only operational/emergency powers and its own signer
        // set. This guard makes the boundary explicit and un-bypassable — a
        // signer majority cannot quietly override a governance decision on a
        // governed parameter.
        if governance::governance_enabled(env) && governance::is_governable_action(&proposal.action)
        {
            return Err(EscrowError::GovernanceRequired);
        }

        match proposal.action.clone() {
            AdminAction::Pause => {
                env.storage().instance().set(&DataKey::Paused, &true);
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("paused")),
                    Paused {
                        paused_by: proposal.proposer.clone()
                    },
                );
            }
            AdminAction::Unpause => {
                env.storage().instance().set(&DataKey::Paused, &false);
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("unpaused")),
                    Unpaused {
                        unpaused_by: proposal.proposer.clone()
                    },
                );
            }
            AdminAction::SetFeeBps(fee) => {
                if fee > MAX_FEE_BPS {
                    return Err(EscrowError::InvalidFee);
                }
                env.storage().instance().set(&symbol_short!("FEE"), &fee);
            }
            AdminAction::SetTreasury(treasury) => {
                env.storage()
                    .instance()
                    .set(&symbol_short!("TRE"), &treasury);
            }
            AdminAction::AddSigner(signer) => {
                let mut signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                // A redundant add used to fall through silently and still mark the
                // proposal executed, so nothing downstream could tell it apart from
                // a real signer-set change. Fail loudly instead (issue #1155).
                if signers.iter().any(|s| s == signer) {
                    return Err(EscrowError::AlreadyInitialized);
                }
                signers.push_back(signer);
                env.storage()
                    .instance()
                    .set(&DataKey::MultiSigSigners, &signers);
            }
            AdminAction::RemoveSigner(signer) => {
                let mut signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                let threshold: u32 = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigThreshold)
                    .unwrap_or(1);

                // Removing an address that is not a signer is a no-op, not a
                // success — report it the same way `RotateSigner` already does
                // for an unknown old signer (issue #1155).
                let idx = signers
                    .iter()
                    .position(|s| s == signer)
                    .ok_or(EscrowError::SignerNotFound)?;
                if signers.len() <= threshold {
                    return Err(EscrowError::InvalidThreshold);
                }
                signers.remove(idx as u32);
                env.storage()
                    .instance()
                    .set(&DataKey::MultiSigSigners, &signers);
            }
            AdminAction::ChangeThreshold(new_threshold) => {
                let signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                if new_threshold == 0 || new_threshold > signers.len() {
                    return Err(EscrowError::InvalidThreshold);
                }
                env.storage()
                    .instance()
                    .set(&DataKey::MultiSigThreshold, &new_threshold);
            }
            AdminAction::RotateSigner(old_signer, new_signer) => {
                let mut signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                if let Some(idx) = signers.iter().position(|s| s == old_signer) {
                    signers.set(idx as u32, new_signer);
                    env.storage()
                        .instance()
                        .set(&DataKey::MultiSigSigners, &signers);
                } else {
                    return Err(EscrowError::SignerNotFound);
                }
            }
            AdminAction::EmergencyWithdraw(job_id, recipient) => {
                // Only callable while the contract is paused.
                let paused: bool = env
                    .storage()
                    .instance()
                    .get(&DataKey::Paused)
                    .unwrap_or(false);
                if !paused {
                    return Err(EscrowError::ContractNotPaused);
                }

                let mut job: Job = env
                    .storage()
                    .persistent()
                    .get(&get_job_key(job_id))
                    .ok_or(EscrowError::JobNotFound)?;
                bump_job_ttl(&env, job_id);

                // Distinguish between the two payment models for Approved milestones:
                //
                // - Immediate-payment model (`release_milestone` / `finalize_inactivity_approval`
                //   / `release_partial_payment`): funds were already disbursed to the freelancer,
                //   so `MilestoneDisbursed` > 0. These amounts are genuinely gone from escrow and
                //   must NOT be withdrawn by admin (would be a double-spend on re-funded balance).
                //
                // - Deferred-payment model (`approve_milestone` / `approve_milestones_batch`):
                //   the milestone is marked Approved but NO funds have left the contract yet
                //   (`MilestoneDisbursed` == 0). If we simply exclude these amounts from
                //   `withdrawable` and then cancel the job, the funds are permanently stranded
                //   because `complete_job` (the only normal payout path) can never run on a
                //   Cancelled job. We must pay the freelancer for these milestones here.
                //
                // `get_milestone_disbursed` returns the cumulative amount already paid out via
                // the immediate-payment path; 0 means no payment has left escrow for that
                // milestone, regardless of its Approved status.
                let token_client = token::Client::new(&env, &job.token);

                // Amount of deferred-approved milestone funds that must be paid to the
                // freelancer as part of this emergency action (default token only; multi-token
                // deferred milestones are handled per-token in the token_balances loop below).
                let mut deferred_to_freelancer: i128 = 0;

                // Amount of already-disbursed milestone funds (immediate model); these are
                // already out of escrow, so we exclude them from what the admin can withdraw.
                let mut already_paid: i128 = 0;

                for ms in job.milestones.iter() {
                    if ms.status != MilestoneStatus::Approved {
                        continue;
                    }
                    // Only count default-token milestones here; token_balances loop handles
                    // the rest for multi-token jobs.
                    if ms.token.is_some() {
                        continue;
                    }
                    let disbursed = get_milestone_disbursed(&env, job_id, ms.id);
                    if disbursed > 0 {
                        // Immediate model: funds already left escrow. Exclude from withdrawable.
                        already_paid = already_paid.saturating_add(ms.amount);
                    } else {
                        // Deferred model: funds still sit in escrow but are owed to the
                        // freelancer. Pay them out now before the job becomes Cancelled.
                        deferred_to_freelancer = deferred_to_freelancer.saturating_add(ms.amount);
                    }
                }

                // Remaining escrowed default-token balance available for admin withdrawal:
                // total funded minus what's already been paid (immediate model) minus what
                // we are about to pay the freelancer (deferred model).
                let withdrawable = job
                    .total_amount
                    .saturating_sub(already_paid)
                    .saturating_sub(deferred_to_freelancer);

                // At least one of deferred_to_freelancer or withdrawable must be > 0
                // for there to be any action to take.
                if deferred_to_freelancer <= 0 && withdrawable <= 0 {
                    return Err(EscrowError::NoFundsToWithdraw);
                }

                // Only operate on jobs that actually hold escrowed funds.
                if job.status == JobStatus::Funded
                    || job.status == JobStatus::InProgress
                    || job.status == JobStatus::Disputed
                {
                    // Pay deferred-approved milestone funds to the freelancer first.
                    if deferred_to_freelancer > 0 {
                        token_client.transfer(
                            &env.current_contract_address(),
                            &job.freelancer,
                            &deferred_to_freelancer,
                        );
                    }

                    // Withdraw the remaining default-token escrow balance to the admin recipient.
                    if withdrawable > 0 {
                        token_client.transfer(
                            &env.current_contract_address(),
                            &recipient,
                            &withdrawable,
                        );
                    }

                    // For multi-token jobs, apply the same deferred-vs-immediate distinction
                    // per non-default token bucket.
                    let tb_len = job.token_balances.len();
                    for idx in 0..tb_len {
                        let tb = job.token_balances.get(idx).unwrap();
                        if tb.funded_amount <= 0 {
                            continue;
                        }
                        let tb_token_client = token::Client::new(&env, &tb.token);

                        // Identify deferred-approved amounts for this non-default token.
                        let mut tb_deferred: i128 = 0;
                        let mut tb_already_paid: i128 = 0;
                        for ms in job.milestones.iter() {
                            if ms.status != MilestoneStatus::Approved {
                                continue;
                            }
                            if ms.token.as_ref() != Some(&tb.token) {
                                continue;
                            }
                            let disbursed = get_milestone_disbursed(&env, job_id, ms.id);
                            if disbursed > 0 {
                                tb_already_paid = tb_already_paid.saturating_add(ms.amount);
                            } else {
                                tb_deferred = tb_deferred.saturating_add(ms.amount);
                            }
                        }

                        // Pay deferred amounts to freelancer for this token.
                        if tb_deferred > 0 {
                            tb_token_client.transfer(
                                &env.current_contract_address(),
                                &job.freelancer,
                                &tb_deferred,
                            );
                        }

                        // Withdraw remaining funded balance (minus already-paid and deferred).
                        let tb_withdrawable = tb
                            .funded_amount
                            .saturating_sub(tb_already_paid)
                            .saturating_sub(tb_deferred);
                        if tb_withdrawable > 0 {
                            tb_token_client.transfer(
                                &env.current_contract_address(),
                                &recipient,
                                &tb_withdrawable,
                            );
                        }
                    }
                } else {
                    return Err(EscrowError::NoFundsToWithdraw);
                }

                job.status = JobStatus::Cancelled;
                env.storage().persistent().set(&get_job_key(job_id), &job);
                bump_job_ttl(&env, job_id);

                env.events().publish(
                    (symbol_short!("escrow"), Symbol::new(&env, "emrg_wdrw")),
                    (job_id, recipient, withdrawable, job.client, job.freelancer),
                );
            }
        }

        proposal.executed = true;
        // Terminal state: retire the proposal from instance storage so it stops
        // being loaded on every invocation of this contract (issue #1153).
        archive_proposal(env, proposal_id, &proposal);

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("executed")),
            (proposal_id, proposal.action),
        );

        Ok(())
    }

    /// Creates a new job with milestones. Client specifies the freelancer and token for payment.
    pub fn create_job(
        env: Env,
        client: Address,
        freelancer: Address,
        token: Address,
        milestones: Vec<(String, i128, u64)>,
        job_deadline: u64,
        auto_refund_after: u64,
        expiry_ledger: u32,
    ) -> Result<u64, EscrowError> {
        require_not_paused(&env)?;

        let allowed_tokens = Self::get_allowed_tokens(env.clone());
        if !allowed_tokens.is_empty()
            && !allowed_tokens
                .iter()
                .any(|allowed| allowed == token.clone())
        {
            return Err(EscrowError::TokenNotAllowed);
        }

        client.require_auth();

        // Self-employment escrows (client == freelancer) are not allowed: a user
        // controlling both addresses could artificially generate review-eligible
        // jobs. `EscrowError` is at the SDK's 50-variant cap, so this reuses
        // `Unauthorized` rather than adding a new variant.
        if client == freelancer {
            return Err(EscrowError::Unauthorized);
        }

        if job_deadline <= env.ledger().timestamp() {
            return Err(EscrowError::InvalidDeadline);
        }

        if expiry_ledger <= env.ledger().sequence() {
            return Err(EscrowError::InvalidDeadline);
        }

        if milestones.is_empty() {
            return Err(EscrowError::EmptyMilestones);
        }
        if milestones.len() > MAX_MILESTONES {
            return Err(EscrowError::TooManyMilestones);
        }

        let mut job_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::JobCount)
            .unwrap_or(0);
        job_count += 1;

        let mut total: i128 = 0;
        let mut milestone_vec: Vec<Milestone> = Vec::new(&env);
        let mut prev_deadline: u64 = 0;

        for (i, m) in milestones.iter().enumerate() {
            let (desc, amount, deadline) = m;
            if amount <= 0 {
                return Err(EscrowError::InvalidMilestone);
            }
            if deadline <= env.ledger().timestamp() {
                return Err(EscrowError::MilestoneDeadlineInPast);
            }
            if deadline > job_deadline {
                return Err(EscrowError::InvalidDeadline);
            }
            if i > 0 && deadline <= prev_deadline {
                return Err(EscrowError::MilestoneDeadlinesNotOrdered);
            }
            prev_deadline = deadline;
            total += amount;
            if total > i128::MAX / 2 {
                return Err(EscrowError::InvalidMilestone);
            }
            milestone_vec.push_back(Milestone {
                id: i as u32,
                description: desc,
                amount,
                status: MilestoneStatus::Pending,
                deadline,
                token: None,
            });
        }

        let job = Job {
            id: job_count,
            client: client.clone(),
            freelancer: freelancer.clone(),
            token: token.clone(),
            total_amount: total,
            funded_amount: 0,
            status: JobStatus::Created,
            milestones: milestone_vec,
            job_deadline,
            auto_refund_after,
            expiry_ledger,
            token_balances: Vec::new(&env),
        };

        env.storage()
            .persistent()
            .set(&get_job_key(job_count), &job);
        bump_job_ttl(&env, job_count);
        env.storage().instance().set(&DataKey::JobCount, &job_count);
        bump_job_count_ttl(&env);

        // Emit event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("created")),
            (job_count, client, freelancer, token.clone(), total),
        );

        Ok(job_count)
    }

    /// Creates a new multi-token job where each milestone can use a different payment token.
    ///
    /// Milestones with `token: None` fall back to the job-level `token` (the default).
    /// Milestones with `token: Some(addr)` must be in the allowed-tokens list.
    ///
    /// The job-level `total_amount` tracks only the milestones denominated in the default
    /// token. Non-default-token totals are tracked in `Job.token_balances`.
    ///
    /// # Errors
    /// Same as `create_job`, plus:
    /// * `MilestoneTokenNotAllowed` — a milestone's token is not in the allowed list.
    pub fn create_multi_token_job(
        env: Env,
        client: Address,
        freelancer: Address,
        token: Address,
        milestones: Vec<(String, i128, u64, Option<Address>)>,
        job_deadline: u64,
        auto_refund_after: u64,
        expiry_ledger: u32,
    ) -> Result<u64, EscrowError> {
        require_not_paused(&env)?;

        let allowed_tokens = Self::get_allowed_tokens(env.clone());
        if !allowed_tokens.is_empty()
            && !allowed_tokens.iter().any(|a| a == token.clone())
        {
            return Err(EscrowError::TokenNotAllowed);
        }

        client.require_auth();

        // Self-employment escrows (client == freelancer) are not allowed: a user
        // controlling both addresses could artificially generate review-eligible
        // jobs. Same guard as `create_job` (issue #988).
        if client == freelancer {
            return Err(EscrowError::Unauthorized);
        }

        if job_deadline <= env.ledger().timestamp() {
            return Err(EscrowError::InvalidDeadline);
        }
        if expiry_ledger <= env.ledger().sequence() {
            return Err(EscrowError::InvalidDeadline);
        }
        if milestones.is_empty() {
            return Err(EscrowError::EmptyMilestones);
        }
        if milestones.len() > MAX_MILESTONES {
            return Err(EscrowError::TooManyMilestones);
        }

        let mut job_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::JobCount)
            .unwrap_or(0);
        job_count += 1;

        let mut default_total: i128 = 0;
        let mut milestone_vec: Vec<Milestone> = Vec::new(&env);
        let mut token_balances: Vec<TokenBalance> = Vec::new(&env);
        let mut prev_deadline: u64 = 0;

        for (i, m) in milestones.iter().enumerate() {
            let (desc, amount, deadline, ms_token) = m;
            if amount <= 0 {
                return Err(EscrowError::InvalidMilestone);
            }
            if deadline <= env.ledger().timestamp() {
                return Err(EscrowError::MilestoneDeadlineInPast);
            }
            if deadline > job_deadline {
                return Err(EscrowError::InvalidDeadline);
            }
            if i > 0 && deadline <= prev_deadline {
                return Err(EscrowError::MilestoneDeadlinesNotOrdered);
            }
            prev_deadline = deadline;

            if let Some(ref ms_tok) = ms_token {
                if !allowed_tokens.is_empty()
                    && !allowed_tokens.iter().any(|a| a == ms_tok.clone())
                {
                    return Err(EscrowError::TokenNotAllowed);
                }
                if ms_tok == &token {
                    // Treat same-as-default as None for accounting simplicity.
                    default_total = default_total.checked_add(amount).ok_or(EscrowError::InvalidMilestone)?;
                    milestone_vec.push_back(Milestone {
                        id: i as u32,
                        description: desc,
                        amount,
                        status: MilestoneStatus::Pending,
                        deadline,
                        token: None,
                    });
                } else {
                    // Accumulate into the non-default token balance.
                    let mut found = false;
                    let len = token_balances.len();
                    for idx in 0..len {
                        let mut tb = token_balances.get(idx).unwrap();
                        if tb.token == *ms_tok {
                            tb.total_amount = tb.total_amount.checked_add(amount).ok_or(EscrowError::InvalidMilestone)?;
                            token_balances.set(idx, tb);
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        token_balances.push_back(TokenBalance {
                            token: ms_tok.clone(),
                            total_amount: amount,
                            funded_amount: 0,
                        });
                    }
                    milestone_vec.push_back(Milestone {
                        id: i as u32,
                        description: desc,
                        amount,
                        status: MilestoneStatus::Pending,
                        deadline,
                        token: ms_token.clone(),
                    });
                }
            } else {
                default_total = default_total.checked_add(amount).ok_or(EscrowError::InvalidMilestone)?;
                milestone_vec.push_back(Milestone {
                    id: i as u32,
                    description: desc,
                    amount,
                    status: MilestoneStatus::Pending,
                    deadline,
                    token: None,
                });
            }
        }

        let job = Job {
            id: job_count,
            client: client.clone(),
            freelancer: freelancer.clone(),
            token: token.clone(),
            total_amount: default_total,
            funded_amount: 0,
            status: JobStatus::Created,
            milestones: milestone_vec,
            job_deadline,
            auto_refund_after,
            expiry_ledger,
            token_balances,
        };

        env.storage()
            .persistent()
            .set(&get_job_key(job_count), &job);
        bump_job_ttl(&env, job_count);
        env.storage().instance().set(&DataKey::JobCount, &job_count);
        bump_job_count_ttl(&env);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("created")),
            (job_count, client, freelancer, token.clone(), default_total),
        );

        Ok(job_count)
    }

    /// Fund the escrow for a job. The client transfers the total amount to this contract.
    ///
    /// `agreed_value_stroops` is the off-chain-agreed job value expressed in XLM
    /// stroops. When it is non-zero, the contract queries the configured price
    /// oracle for a TWAP of `job.token` and rejects the deposit with
    /// [`EscrowError::InsufficientValue`] if its value falls below the agreed
    /// value minus `max_slippage_bps`. Passing `agreed_value_stroops = 0` bypasses
    /// the oracle check (e.g. for native-XLM jobs and the legacy migration path).
    ///
    /// For multi-token jobs (created via `create_multi_token_job`), this function
    /// automatically collects all non-default token amounts in addition to the default
    /// token. Oracle validation applies to the default token only; use
    /// `validate_token_value` separately for non-default tokens if required.
    pub fn fund_job(
        env: Env,
        job_id: u64,
        client: Address,
        agreed_value_stroops: i128,
        max_slippage_bps: u32,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // Require auth from the persisted job owner to prevent third-party funding.
        job.client.require_auth();

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Job must be in Created state to be funded
        require_state_created(&job)?;

        // Validate that total_amount matches the sum of default-token milestone amounts.
        // Guards against any inconsistency between the two fields (e.g. from a
        // revision path bug) that would leave milestones unpayable or trap surplus funds.
        let milestone_sum: i128 = job
            .milestones
            .iter()
            .filter(|m| m.token.is_none())
            .try_fold(0i128, |acc, m| acc.checked_add(m.amount))
            .ok_or(EscrowError::InvalidAmount)?;
        if job.total_amount != milestone_sum {
            return Err(EscrowError::InvalidAmount);
        }

        // Exchange-rate parity check (no-op when agreed_value_stroops == 0).
        let snapshot = validate_deposit_value(
            &env,
            &job.token,
            job.total_amount,
            agreed_value_stroops,
            max_slippage_bps,
        )?;

        // Transfer the default token (may be zero for fully multi-token jobs).
        if job.total_amount > 0 {
            let token_client = token::Client::new(&env, &job.token);
            token_client.transfer(&client, &env.current_contract_address(), &job.total_amount);
        }
        job.funded_amount = job.total_amount;

        // Transfer each non-default token for multi-token jobs.
        let tb_len = job.token_balances.len();
        for idx in 0..tb_len {
            let mut tb = job.token_balances.get(idx).unwrap();
            if tb.total_amount > 0 {
                token::Client::new(&env, &tb.token).transfer(
                    &client,
                    &env.current_contract_address(),
                    &tb.total_amount,
                );
                tb.funded_amount = tb.total_amount;
                job.token_balances.set(idx, tb);
            }
        }

        job.status = JobStatus::Funded;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Persist the parity snapshot for audit / UI when the oracle was consulted.
        if agreed_value_stroops != 0 {
            env.storage()
                .persistent()
                .set(&DataKey::RateSnapshot(job_id), &snapshot);
            env.storage().persistent().extend_ttl(
                &DataKey::RateSnapshot(job_id),
                TTL_THRESHOLD_LEDGERS,
                TTL_EXTEND_TO_LEDGERS,
            );
        }

        // Emit event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("funded")),
            (job_id, client, job.freelancer, job.token, job.total_amount),
        );

        Ok(())
    }

    /// Incrementally top up the escrow balance for an already-funded job.
    ///
    /// Useful when a revision proposal has increased `total_amount` and the
    /// client wants to pay the difference in multiple instalments rather than
    /// a single lump sum.  The job status is **not** changed by this call.
    ///
    /// # Errors
    /// - `JobNotFound`      — no job with the given id
    /// - `Unauthorized`     — caller is not the job client
    /// - `InvalidStatus`    — job is not Funded or InProgress
    /// - `AlreadyFunded`    — adding `amount` would exceed `total_amount`
    /// - `ContractPaused`   — the contract is paused
    /// - `InvalidAmount`    — `amount` is zero or negative
    pub fn top_up_escrow(
        env: Env,
        client: Address,
        job_id: u64,
        amount: i128,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        client.require_auth();

        if amount <= 0 {
            return Err(EscrowError::InvalidAmount);
        }

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // Multi-token jobs are always fully funded in one shot by fund_job, so there
        // is no legitimate top-up path for them. Guard explicitly so this stays true
        // even if revision support or other total_amount-raising paths are later added.
        if !job.token_balances.is_empty() {
            return Err(EscrowError::InvalidStatus);
        }

        // STATE VALIDATION: Job must be Funded or InProgress to top up
        require_state_funded_or_in_progress(&job)?;

        let new_funded = job
            .funded_amount
            .checked_add(amount)
            .ok_or(EscrowError::AlreadyFunded)?;

        if new_funded > job.total_amount {
            return Err(EscrowError::AlreadyFunded);
        }

        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&client, &env.current_contract_address(), &amount);

        job.funded_amount = new_funded;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("top_up")),
            (job_id, client, amount, new_funded),
        );

        Ok(())
    }

    /// Called by the dispute contract to resolve a disputed job and distribute funds.
    /// Uses the full DisputeResolution enum to correctly handle every outcome,
    /// including the zero-remaining edge case where only the job status needs updating.
    ///
    /// Outcomes: `FreelancerWins` → `Completed`; `ClientWins`, `RefundBoth`,
    /// `RefundSplit` and `MaliciousFiling` → `Cancelled`; `Escalate` → **no state
    /// change and no payout** — the job stays in its current (`Disputed`) state with
    /// the escrow intact, awaiting either another call to this function with a final
    /// resolution or `expire_job` after the deadline. See the [`JobStatus`]
    /// state-machine docs, "Escalated disputes", for the full rationale.
    ///
    /// Because [`require_state_disputable`] accepts `Disputed`, this function is
    /// deliberately re-callable: escalation is a hand-off to a higher arbitration
    /// tier, not a final answer.
    pub fn resolve_dispute_callback(
        env: Env,
        job_id: u64,
        resolution: DisputeResolution,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;

        // STATE VALIDATION: Job must be in a disputable state
        require_state_disputable(&job)?;

        // Calculate per-token remaining amounts for multi-token support.
        // "remaining" for a token = total - sum(Approved milestones in that token).
        let default_approved: i128 = job
            .milestones
            .iter()
            .filter(|m| m.token.is_none() && m.status == MilestoneStatus::Approved)
            .map(|m| m.amount)
            .sum();
        let default_remaining = job.total_amount - default_approved;

        let client_addr = job.client.clone();
        let freelancer_addr = job.freelancer.clone();
        let treasury: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TRE"))
            .unwrap_or(client_addr.clone());

        // Distribute one token according to the resolution.
        apply_dispute_distribution(
            &env,
            &job.token,
            default_remaining,
            &resolution,
            &client_addr,
            &freelancer_addr,
            &treasury,
        );

        // Distribute non-default tokens.
        let tb_len = job.token_balances.len();
        for idx in 0..tb_len {
            let tb = job.token_balances.get(idx).unwrap();
            let tb_approved: i128 = job
                .milestones
                .iter()
                .filter(|m| m.token.as_ref() == Some(&tb.token) && m.status == MilestoneStatus::Approved)
                .map(|m| m.amount)
                .sum();
            let tb_remaining = tb.total_amount - tb_approved;
            apply_dispute_distribution(
                &env,
                &tb.token,
                tb_remaining,
                &resolution,
                &client_addr,
                &freelancer_addr,
                &treasury,
            );
        }

        // Update job status based on resolution.
        match resolution {
            DisputeResolution::ClientWins
            | DisputeResolution::RefundBoth
            | DisputeResolution::RefundSplit(_)
            | DisputeResolution::MaliciousFiling => {
                job.status = JobStatus::Cancelled;
            }
            DisputeResolution::FreelancerWins => {
                job.status = JobStatus::Completed;
            }
            DisputeResolution::Escalate => {
                // Leave status unchanged: the job stays Disputed (a self-loop) with
                // the escrow untouched. Escalation hands the case to a higher
                // arbitration tier, which is expected to call this function again
                // with a final resolution; `expire_job` remains the backstop once
                // the deadline passes. Documented on `JobStatus` under
                // "Escalated disputes".
            }
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("dispute")),
            (job_id, resolution, job.client, job.freelancer, job.token),
        );

        Ok(())
    }

    /// Pure payout breakdown for a dispute resolution. No state is read or
    /// written — safe for frontend preview without a transaction.
    /// All four amounts always sum to `escrow_amount` (no dust).
    pub fn calculate_payout(
        _env: Env,
        escrow_amount: i128,
        platform_fee_bps: u32,
        arbitrator_fee_bps: u32,
        outcome: DisputeResolution,
    ) -> PayoutBreakdown {
        let platform_fee = (escrow_amount * platform_fee_bps as i128) / 10_000;
        let arbitrator_fee = (escrow_amount * arbitrator_fee_bps as i128) / 10_000;
        let remaining = escrow_amount - platform_fee - arbitrator_fee;

        match outcome {
            DisputeResolution::ClientWins => PayoutBreakdown {
                client: remaining,
                freelancer: 0,
                platform: platform_fee,
                arbitrators: arbitrator_fee,
            },
            DisputeResolution::FreelancerWins => PayoutBreakdown {
                client: 0,
                freelancer: remaining,
                platform: platform_fee,
                arbitrators: arbitrator_fee,
            },
            DisputeResolution::RefundBoth => {
                let half = remaining / 2;
                PayoutBreakdown {
                    client: half,
                    freelancer: remaining - half,
                    platform: platform_fee,
                    arbitrators: arbitrator_fee,
                }
            }
            DisputeResolution::RefundSplit(pct_client) => {
                let pct = if pct_client > 100 { 100 } else { pct_client } as i128;
                let client_amount = (remaining * pct) / 100;
                PayoutBreakdown {
                    client: client_amount,
                    freelancer: remaining - client_amount,
                    platform: platform_fee,
                    arbitrators: arbitrator_fee,
                }
            }
            DisputeResolution::Escalate => PayoutBreakdown {
                client: 0,
                freelancer: 0,
                platform: 0,
                arbitrators: 0,
            },
            DisputeResolution::MaliciousFiling => PayoutBreakdown {
                client: 0,
                freelancer: 0,
                platform: remaining + platform_fee,
                arbitrators: arbitrator_fee,
            },
        }
    }

    /// Freelancer submits a milestone as completed.
    pub fn submit_milestone(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        freelancer: Address,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        freelancer.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.freelancer != freelancer {
            return Err(EscrowError::Unauthorized);
        }
        
        // STATE VALIDATION: Job must be Funded or InProgress, and not disputed
        require_state_funded_or_in_progress(&job)?;
        require_state_not_disputed(&job)?;

        let mut milestones = job.milestones.clone();
        let milestone = milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;

        if milestone.status != MilestoneStatus::Pending
            && milestone.status != MilestoneStatus::InProgress
        {
            return Err(EscrowError::InvalidStatus);
        }

        if env.ledger().timestamp() > milestone.deadline {
            return Err(EscrowError::MilestoneDeadlineExceeded);
        }

        let updated = Milestone {
            id: milestone.id,
            description: milestone.description.clone(),
            amount: milestone.amount,
            status: MilestoneStatus::Submitted,
            deadline: milestone.deadline,
            token: milestone.token.clone(),
        };
        milestones.set(milestone_id, updated);

        job.milestones = milestones;
        job.status = JobStatus::InProgress;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        let submitted_key = DataKey::MilestoneSubmittedAt(job_id, milestone_id);
        env.storage()
            .persistent()
            .set(&submitted_key, &env.ledger().timestamp());
        env.storage().persistent().extend_ttl(
            &submitted_key,
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        let auto_key = DataKey::InactivityAutoApproveAt(job_id, milestone_id);
        if env.storage().persistent().has(&auto_key) {
            env.storage().persistent().remove(&auto_key);
        }

        Ok(())
    }

    /// Client approves a milestone and releases payment to the freelancer.
    pub fn approve_milestone(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        client: Address,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Cannot approve milestones while disputed
        require_state_not_disputed(&job)?;

        let mut milestones = job.milestones.clone();
        let milestone = milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;

        if milestone.status != MilestoneStatus::Submitted {
            return Err(EscrowError::InvalidStatus);
        }

        let updated = Milestone {
            id: milestone.id,
            description: milestone.description.clone(),
            amount: milestone.amount,
            status: MilestoneStatus::Approved,
            deadline: milestone.deadline,
            token: milestone.token.clone(),
        };
        milestones.set(milestone_id, updated);
        job.milestones = milestones.clone();

        // Keep the job in InProgress; complete_job will finalize payment and transition to Completed.
        if job.status != JobStatus::InProgress {
            job.status = JobStatus::InProgress;
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        let submitted_key = DataKey::MilestoneSubmittedAt(job_id, milestone_id);
        if env.storage().persistent().has(&submitted_key) {
            env.storage().persistent().remove(&submitted_key);
        }
        let auto_key = DataKey::InactivityAutoApproveAt(job_id, milestone_id);
        if env.storage().persistent().has(&auto_key) {
            env.storage().persistent().remove(&auto_key);
        }

        // Emit milestone approved event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("milestone")),
            (
                job_id,
                milestone_id,
                client,
                job.freelancer.clone(),
                milestone.amount,
            ),
        );

        Ok(())
    }

    /// Client approves multiple milestones at once and releases payments to the freelancer.
    /// All milestone indices must be in Submitted state before any state changes occur.
    /// If any index is invalid or not in Submitted state, the entire call reverts.
    pub fn approve_milestones_batch(
        env: Env,
        job_id: u64,
        milestone_indices: Vec<u32>,
        client: Address,
    ) -> Result<i128, EscrowError> {
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Cannot approve milestones while disputed
        require_state_not_disputed(&job)?;

        // Validate all milestone indices before making any state changes.
        // Duplicate indices are rejected here so a repeated index can't be
        // counted (and its amount summed into total_released) more than once
        // while only transitioning to Approved a single time.
        let mut milestones = job.milestones.clone();
        let mut total_released: i128 = 0;
        let mut seen = [false; MAX_MILESTONES as usize];

        for i in milestone_indices.iter() {
            let index = i;
            let milestone = milestones
                .get(index)
                .ok_or(EscrowError::MilestoneNotFound)?;

            // Safe: milestones.len() <= MAX_MILESTONES, and the get() above
            // already confirmed index < milestones.len().
            let idx = index as usize;
            if seen[idx] {
                return Err(EscrowError::InvalidMilestoneIndex);
            }
            seen[idx] = true;

            if milestone.status != MilestoneStatus::Submitted {
                return Err(EscrowError::InvalidStatus);
            }
        }

        // All validations passed - now process the batch atomically
        for i in milestone_indices.iter() {
            let index = i;
            let milestone = milestones.get(index).unwrap();

            // Release payment for this milestone
            total_released += milestone.amount;

            let updated = Milestone {
                id: milestone.id,
                description: milestone.description.clone(),
                amount: milestone.amount,
                status: MilestoneStatus::Approved,
                deadline: milestone.deadline,
                token: milestone.token.clone(),
            };
            milestones.set(index, updated);

            let submitted_key = DataKey::MilestoneSubmittedAt(job_id, index);
            if env.storage().persistent().has(&submitted_key) {
                env.storage().persistent().remove(&submitted_key);
            }
            let auto_key = DataKey::InactivityAutoApproveAt(job_id, index);
            if env.storage().persistent().has(&auto_key) {
                env.storage().persistent().remove(&auto_key);
            }
        }

        job.milestones = milestones.clone();

        // Keep the job in InProgress; complete_job will finalize payment and transition to Completed.
        if job.status != JobStatus::InProgress {
            job.status = JobStatus::InProgress;
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit batch approval event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("batch")),
            (
                job_id,
                milestone_indices,
                total_released,
                job.client.clone(),
                job.freelancer.clone(),
            ),
        );

        Ok(total_released)
    }

    /// After a milestone is submitted, allow either party to trigger an inactivity-based
    /// auto-approval flow when the client is unresponsive for a threshold duration.
    pub fn trigger_inactivity_extension(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        caller: Address,
    ) -> Result<u64, EscrowError> {
        bump_escrow_ttl(&env, job_id);
        caller.require_auth();
        require_not_paused(&env)?;

        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::Unauthorized);
        }

        let milestone = job
            .milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;
        if milestone.status != MilestoneStatus::Submitted {
            return Err(EscrowError::InvalidStatus);
        }

        let submitted_key = DataKey::MilestoneSubmittedAt(job_id, milestone_id);
        let submitted_at: u64 = env
            .storage()
            .persistent()
            .get(&submitted_key)
            .ok_or(EscrowError::InvalidStatus)?;

        let now = env.ledger().timestamp();
        if now < submitted_at.saturating_add(INACTIVITY_THRESHOLD_SECS) {
            return Err(EscrowError::InvalidStatus);
        }

        let auto_approve_at = now.saturating_add(INACTIVITY_GRACE_SECS);
        let auto_key = DataKey::InactivityAutoApproveAt(job_id, milestone_id);
        env.storage().persistent().set(&auto_key, &auto_approve_at);
        env.storage().persistent().extend_ttl(
            &auto_key,
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "inact_trig")),
            (job_id, milestone_id, caller, auto_approve_at),
        );

        Ok(auto_approve_at)
    }

    /// Finalize an inactivity-triggered auto-approval after the grace period.
    /// Pays out the freelancer for the milestone amount and updates job status as needed.
    pub fn finalize_inactivity_approval(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        caller: Address,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        caller.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Cannot finalize inactivity approval while disputed
        require_state_not_disputed(&job)?;

        let mut milestones = job.milestones.clone();
        let milestone = milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;
        if milestone.status != MilestoneStatus::Submitted {
            return Err(EscrowError::InvalidStatus);
        }

        let submitted_key = DataKey::MilestoneSubmittedAt(job_id, milestone_id);
        let submitted_at: u64 = env
            .storage()
            .persistent()
            .get(&submitted_key)
            .ok_or(EscrowError::InvalidStatus)?;

        let now = env.ledger().timestamp();
        if now < submitted_at.saturating_add(INACTIVITY_THRESHOLD_SECS) {
            return Err(EscrowError::InvalidStatus);
        }

        let auto_key = DataKey::InactivityAutoApproveAt(job_id, milestone_id);
        let auto_approve_at: u64 = env
            .storage()
            .persistent()
            .get(&auto_key)
            .ok_or(EscrowError::InvalidStatus)?;
        if now < auto_approve_at {
            return Err(EscrowError::InvalidStatus);
        }

        // Release payment for this milestone (same logic as client approval).
        let ms_token = resolve_milestone_token(&milestone, &job);
        let token_client = token::Client::new(&env, &ms_token);

        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&symbol_short!("FEE"))
            .unwrap_or(0);
        let treasury: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TRE"))
            .unwrap_or(env.current_contract_address());

        let fee_amount = (milestone.amount * fee_bps as i128) / 10_000;
        let freelancer_amount = milestone.amount - fee_amount;

        if fee_amount > 0 {
            token_client.transfer(&env.current_contract_address(), &treasury, &fee_amount);
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("fee")),
                (job_id, milestone_id, fee_amount, treasury.clone()),
            );
        }

        // Split payout between main freelancer and sub-freelancer when an active
        // sub-assignment exists for this milestone.
        let sub_key = DataKey::SubAssignment(job_id, milestone_id);
        let sub_opt: Option<SubAssignment> = env.storage().persistent().get(&sub_key);
        if let Some(mut sub) = sub_opt {
            if sub.status == SubAssignmentStatus::Active {
                let sub_amount = if sub.amount <= freelancer_amount {
                    sub.amount
                } else {
                    freelancer_amount
                };
                let main_amount = freelancer_amount - sub_amount;
                if sub_amount > 0 {
                    token_client.transfer(&env.current_contract_address(), &sub.sub_freelancer, &sub_amount);
                }
                if main_amount > 0 {
                    token_client.transfer(&env.current_contract_address(), &job.freelancer, &main_amount);
                }
                let sub_fl = sub.sub_freelancer.clone();
                sub.status = SubAssignmentStatus::Paid;
                env.storage().persistent().set(&sub_key, &sub);
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("sub_paid")),
                    (job_id, milestone_id, sub_fl, sub_amount),
                );
            } else if freelancer_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
            }
        } else if freelancer_amount > 0 {
            token_client.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
        }

        // Record that the full nominal milestone amount has been disbursed so that
        // EmergencyWithdraw recognises this as the immediate-payment model
        // (MilestoneDisbursed > 0) and does NOT pay the freelancer a second time.
        // release_milestone and release_partial_payment do the same thing; omitting
        // this call here would cause a double-pay if EmergencyWithdraw runs after
        // finalize_inactivity_approval.
        record_milestone_disbursed(&env, job_id, milestone.id, milestone.amount);

        let updated = Milestone {
            id: milestone.id,
            description: milestone.description.clone(),
            amount: milestone.amount,
            status: MilestoneStatus::Approved,
            deadline: milestone.deadline,
            token: milestone.token.clone(),
        };
        milestones.set(milestone_id, updated);
        job.milestones = milestones.clone();

        // Keep the job in InProgress; complete_job will finalize payment and transition to Completed.
        if job.status != JobStatus::InProgress {
            job.status = JobStatus::InProgress;
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        env.storage().persistent().remove(&submitted_key);
        env.storage().persistent().remove(&auto_key);

        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "inact_final")),
            (job_id, milestone_id, caller),
        );

        Ok(())
    }

    /// Client releases a partial payment for a submitted milestone.
    ///
    /// `amount` must be > 0 and <= the milestone's current stored amount.
    /// After the call the milestone's `amount` is reduced by `amount`.
    /// * If the remaining balance reaches 0 the status transitions to `Approved`.
    /// * Otherwise the status is set to `PartiallyPaid` so further payments
    ///   (partial or full) can be made later.
    ///
    /// # Errors
    /// * `Unauthorized`          — caller is not the job's client.
    /// * `InvalidStatus`         — job is disputed, or milestone is not Submitted / PartiallyPaid.
    /// * `InvalidPartialAmount`  — amount <= 0 or amount > milestone.amount.
    pub fn release_partial_payment(
        env: Env,
        job_id: u64,
        milestone_index: u32,
        amount: i128,
        client: Address,
        nonce: u64,
    ) -> Result<(), EscrowError> {
        consume_nonce(&env, &client, &Symbol::new(&env, "partial_pmt"), nonce)?;
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }
        
        // STATE VALIDATION: Cannot release partial payment while disputed
        require_state_not_disputed(&job)?;

        let mut milestones = job.milestones.clone();
        let milestone = milestones
            .get(milestone_index)
            .ok_or(EscrowError::MilestoneNotFound)?;

        // Only allow partial payment on a Submitted or already-PartiallyPaid milestone.
        if milestone.status != MilestoneStatus::Submitted
            && milestone.status != MilestoneStatus::PartiallyPaid
        {
            return Err(EscrowError::InvalidStatus);
        }

        // Validate the requested amount.
        if amount <= 0 || amount > milestone.amount {
            return Err(EscrowError::InvalidPartialAmount);
        }

        let sub_check_key = DataKey::SubAssignment(job_id, milestone_index);
        if let Some(sub) = env
            .storage()
            .persistent()
            .get::<DataKey, SubAssignment>(&sub_check_key)
        {
            if sub.status == SubAssignmentStatus::Active {
                return Err(EscrowError::InvalidStatus);
            }
        }

        // Release partial payment to freelancer using the milestone's own token.
        let ms_token = resolve_milestone_token(&milestone, &job);
        token::Client::new(&env, &ms_token).transfer(
            &env.current_contract_address(),
            &job.freelancer,
            &amount,
        );

        // Track cumulative disbursement so a later revision can't undercut what's
        // already been paid out for this milestone.
        record_milestone_disbursed(&env, job_id, milestone.id, amount);

        // Deduct paid amount from milestone; transition status accordingly.
        let remaining = milestone.amount - amount;
        let new_status = if remaining == 0 {
            MilestoneStatus::Approved
        } else {
            MilestoneStatus::PartiallyPaid
        };

        let updated = Milestone {
            id: milestone.id,
            description: milestone.description.clone(),
            amount: remaining,
            status: new_status,
            deadline: milestone.deadline,
            token: milestone.token.clone(),
        };
        milestones.set(milestone_index, updated);
        job.milestones = milestones.clone();

        // Keep the job in InProgress; complete_job will finalize payment and transition to Completed.
        if job.status != JobStatus::InProgress {
            job.status = JobStatus::InProgress;
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit PartialPaymentReleased event.
        let client = job.client.clone();
        let freelancer = job.freelancer.clone();
        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "partial_pmt")),
            (job_id, milestone_index, amount, client, freelancer),
        );

        Ok(())
    }

    /// Finalize a job when all milestones are approved, deducting the protocol fee
    /// and distributing remaining escrow to the freelancer.
    ///
    /// # Authorization
    /// Either the client or the freelancer may call this function.
    ///
    /// # Errors
    /// * `JobNotFound`   — if the job does not exist
    /// * `Unauthorized`  — if caller is neither client nor freelancer
    /// * `InvalidStatus` — if job is not `InProgress` or milestones are not all `Approved`
    /// * `ContractPaused` — if the contract is paused
    pub fn complete_job(
        env: Env,
        job_id: u64,
        caller: Address,
    ) -> Result<(), EscrowError> {
        caller.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::Unauthorized);
        }

        if job.status != JobStatus::InProgress {
            return Err(EscrowError::InvalidStatus);
        }

        let all_approved = job
            .milestones
            .iter()
            .all(|m| m.status == MilestoneStatus::Approved);
        if !all_approved {
            return Err(EscrowError::InvalidStatus);
        }

        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&symbol_short!("FEE"))
            .unwrap_or(0);
        let fee_recipient: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TRE"))
            .unwrap_or(env.current_contract_address());

        // Pay out each milestone in its own token and emit per-token events,
        // respecting any active sub-assignment.
        // Use Map to aggregate amounts by token address for event emission.
        let mut fee_by_token: Map<Address, i128> = Map::new(&env);
        let mut freelancer_by_token: Map<Address, i128> = Map::new(&env);


        for ms in job.milestones.iter() {
            if ms.amount <= 0 {
                continue;
            }
            let ms_token = resolve_milestone_token(&ms, &job);
            let fee_amount = (ms.amount * fee_bps as i128) / 10_000;
            let freelancer_amount = ms.amount - fee_amount;

            let tc = token::Client::new(&env, &ms_token);
            if fee_amount > 0 {
                tc.transfer(&env.current_contract_address(), &fee_recipient, &fee_amount);
            }

            // Split payout between main freelancer and sub-freelancer when an active
            // sub-assignment exists for this milestone.
            let sub_key = DataKey::SubAssignment(job_id, ms.id);
            let sub_opt: Option<SubAssignment> = env.storage().persistent().get(&sub_key);
            if let Some(mut sub) = sub_opt {
                if sub.status == SubAssignmentStatus::Active {
                    let sub_amount = if sub.amount <= freelancer_amount {
                        sub.amount
                    } else {
                        freelancer_amount
                    };
                    let main_amount = freelancer_amount - sub_amount;
                    if sub_amount > 0 {
                        tc.transfer(&env.current_contract_address(), &sub.sub_freelancer, &sub_amount);
                    }
                    if main_amount > 0 {
                        tc.transfer(&env.current_contract_address(), &job.freelancer, &main_amount);
                    }
                    let sub_fl = sub.sub_freelancer.clone();
                    sub.status = SubAssignmentStatus::Paid;
                    env.storage().persistent().set(&sub_key, &sub);
                    env.events().publish(
                        (symbol_short!("escrow"), symbol_short!("sub_paid")),
                        (job_id, ms.id, sub_fl, sub_amount),
                    );
                } else if freelancer_amount > 0 {
                    tc.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
                }
            } else if freelancer_amount > 0 {
                tc.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
            }

            // Accumulate amounts per token for event emission.
            let current_fee = fee_by_token.get(ms_token.clone()).unwrap_or(0);
            fee_by_token.set(ms_token.clone(), current_fee.saturating_add(fee_amount));

            let current_freelancer = freelancer_by_token.get(ms_token.clone()).unwrap_or(0);
            freelancer_by_token.set(ms_token.clone(), current_freelancer.saturating_add(freelancer_amount));
        }

        job.status = JobStatus::Completed;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit events: maintain backward compatibility for single-token jobs,
        // use per-token format for multi-token jobs.
        let num_tokens = fee_by_token.len();

        if num_tokens == 1 {
            // Single-token job: use legacy event format (job_id, amount, recipient)
            let token_addr = fee_by_token.keys().get(0).unwrap();
            let total_fee = fee_by_token.get(token_addr.clone()).unwrap_or(0);
            let total_freelancer = freelancer_by_token.get(token_addr.clone()).unwrap_or(0);

            env.events().publish(
                (symbol_short!("escrow"), Symbol::new(&env, "fee_taken")),
                (job_id, total_fee, fee_recipient.clone()),
            );

            env.events().publish(
                (symbol_short!("escrow"), Symbol::new(&env, "pmt_released")),
                (job_id, job.freelancer.clone(), total_freelancer),
            );
        } else {
            // Multi-token job: emit per-token events (job_id, token, amount, recipient)
            for token_addr in fee_by_token.keys() {
                let fee_amount = fee_by_token.get(token_addr.clone()).unwrap_or(0);
                if fee_amount > 0 {
                    env.events().publish(
                        (symbol_short!("escrow"), Symbol::new(&env, "fee_taken")),
                        (job_id, token_addr.clone(), fee_amount, fee_recipient.clone()),
                    );
                }
            }

            for token_addr in freelancer_by_token.keys() {
                let freelancer_amount = freelancer_by_token.get(token_addr.clone()).unwrap_or(0);
                if freelancer_amount > 0 {
                    env.events().publish(
                        (symbol_short!("escrow"), Symbol::new(&env, "pmt_released")),
                        (job_id, token_addr.clone(), job.freelancer.clone(), freelancer_amount),
                    );
                }
            }
        }

        Ok(())
    }

    /// Client releases the full payment for a submitted milestone.
    ///
    /// # Authorization
    /// Only the client may call this function.
    ///
    /// # Errors
    /// * `Unauthorized`   — caller is not the job's client
    /// * `InvalidStatus`  — job is disputed, or milestone is not Submitted
    /// * `MilestoneNotFound` — milestone index does not exist
    pub fn release_milestone(
        env: Env,
        job_id: u64,
        milestone_index: u32,
        client: Address,
        min_release_value_stroops: i128,
        nonce: u64,
    ) -> Result<(), EscrowError> {
        consume_nonce(&env, &client, &Symbol::new(&env, "release_ms"), nonce)?;
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }
        if job.status == JobStatus::Disputed {
            return Err(EscrowError::InvalidStatus);
        }

        let mut milestones = job.milestones.clone();
        let milestone = milestones
            .get(milestone_index)
            .ok_or(EscrowError::MilestoneNotFound)?;

        if milestone.status != MilestoneStatus::Submitted {
            return Err(EscrowError::InvalidStatus);
        }

        let ms_token = resolve_milestone_token(&milestone, &job);

        if min_release_value_stroops > 0 {
            let oracle_addr: Address = env
                .storage()
                .instance()
                .get(&DataKey::PriceOracle)
                .ok_or(EscrowError::OracleUnavailable)?;

            let twap: i128 = env.invoke_contract(
                &oracle_addr,
                &Symbol::new(&env, "twap"),
                soroban_sdk::vec![
                    &env,
                    ms_token.clone().into_val(&env),
                    TWAP_SAMPLE_LEDGERS.into_val(&env),
                ],
            );

            if twap <= 0 {
                return Err(EscrowError::OracleUnavailable);
            }

            let current_value = milestone
                .amount
                .checked_mul(twap)
                .ok_or(EscrowError::ValueOverflow)?
                / PRICE_SCALE;

            if current_value < min_release_value_stroops {
                return Err(EscrowError::SlippageExceeded);
            }
        }

        // Compute fee and net freelancer amount.
        let token_client = token::Client::new(&env, &ms_token);
        let fee_bps: u32 = env
            .storage()
            .instance()
            .get(&symbol_short!("FEE"))
            .unwrap_or(0);
        let treasury: Address = env
            .storage()
            .instance()
            .get(&symbol_short!("TRE"))
            .unwrap_or(env.current_contract_address());

        let fee_amount = (milestone.amount * fee_bps as i128) / 10_000;
        let freelancer_amount = milestone.amount - fee_amount;

        if fee_amount > 0 {
            token_client.transfer(&env.current_contract_address(), &treasury, &fee_amount);
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("fee")),
                (job_id, milestone_index, fee_amount, treasury.clone()),
            );
        }

        // Split payout between main freelancer and sub-freelancer when an active
        // sub-assignment exists for this milestone.
        let sub_key = DataKey::SubAssignment(job_id, milestone_index);
        let sub_opt: Option<SubAssignment> = env.storage().persistent().get(&sub_key);
        if let Some(mut sub) = sub_opt {
            if sub.status == SubAssignmentStatus::Active {
                let sub_amount = if sub.amount <= freelancer_amount {
                    sub.amount
                } else {
                    freelancer_amount
                };
                let main_amount = freelancer_amount - sub_amount;
                if sub_amount > 0 {
                    token_client.transfer(&env.current_contract_address(), &sub.sub_freelancer, &sub_amount);
                }
                if main_amount > 0 {
                    token_client.transfer(&env.current_contract_address(), &job.freelancer, &main_amount);
                }
                let sub_fl = sub.sub_freelancer.clone();
                sub.status = SubAssignmentStatus::Paid;
                env.storage().persistent().set(&sub_key, &sub);
                env.events().publish(
                    (symbol_short!("escrow"), symbol_short!("sub_paid")),
                    (job_id, milestone_index, sub_fl, sub_amount),
                );
            } else if freelancer_amount > 0 {
                token_client.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
            }
        } else if freelancer_amount > 0 {
            token_client.transfer(&env.current_contract_address(), &job.freelancer, &freelancer_amount);
        }

        // Track cumulative disbursement (full nominal milestone amount — fee included —
        // since that value has left escrow for good) so a later revision can't undercut it.
        record_milestone_disbursed(&env, job_id, milestone.id, milestone.amount);

        let updated = Milestone {
            id: milestone.id,
            description: milestone.description.clone(),
            amount: milestone.amount,
            status: MilestoneStatus::Approved,
            deadline: milestone.deadline,
            token: milestone.token.clone(),
        };
        milestones.set(milestone_index, updated);
        job.milestones = milestones.clone();

        // Check if all milestones are now fully paid.
        let all_approved = milestones
            .iter()
            .all(|m| m.status == MilestoneStatus::Approved);
        if all_approved {
            job.status = JobStatus::Completed;
        }

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Clean up auxiliary keys.
        let submitted_key = DataKey::MilestoneSubmittedAt(job_id, milestone_index);
        if env.storage().persistent().has(&submitted_key) {
            env.storage().persistent().remove(&submitted_key);
        }
        let auto_key = DataKey::InactivityAutoApproveAt(job_id, milestone_index);
        if env.storage().persistent().has(&auto_key) {
            env.storage().persistent().remove(&auto_key);
        }

        // Emit MilestoneReleased event.
        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "ms_released")),
            (
                job_id,
                milestone_index,
                client,
                job.freelancer.clone(),
                milestone.amount,
            ),
        );

        if all_approved {
            env.events().publish(
                (symbol_short!("escrow"), Symbol::new(&env, "pmt_released")),
                (job_id, job.freelancer, freelancer_amount),
            );
        }

        Ok(())
    }

    /// Cancel a funded job and refund the full escrowed balance back to the client.
    ///
    /// # Authorization
    /// Only the client may cancel. Cancellation is only permitted when the job is in
    /// `Funded` status **and** all milestones are still in `Pending` state — i.e. the
    /// freelancer has not started any work yet.
    ///
    /// # Errors
    /// * `Unauthorized`      — caller is not the job's client.
    /// * `InvalidStatus`     — job is not in `Funded` state (already `Completed`,
    ///                         `Cancelled`, `Created`, or `InProgress`).
    /// * `WorkInProgress`    — at least one milestone is `InProgress` or `Submitted`;
    ///                         the client must open a dispute instead.
    pub fn cancel_job(env: Env, job_id: u64, client: Address, nonce: u64) -> Result<(), EscrowError> {
        consume_nonce(&env, &client, &Symbol::new(&env, "cancel_job"), nonce)?;
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // Only the client may cancel their own job.
        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Job must be in a cancellable state (Funded or InProgress)
        require_state_cancellable(&job)?;
        // Additional guard: explicitly reject if Disputed
        require_state_not_disputed(&job)?;

        // Guard: reject cancellation if any milestone is actively InProgress or Submitted.
        // The client must open a dispute for in-flight work instead.
        let work_started = job.milestones.iter().any(|m| {
            m.status == MilestoneStatus::InProgress || m.status == MilestoneStatus::Submitted
        });
        if work_started {
            return Err(EscrowError::WorkInProgress);
        }

        // Refund the remaining escrowed amount (total minus already-approved milestones).
        // Pay already-approved milestones to the freelancer, since payment only happens in complete_job.
        // For multi-token jobs, operate per distinct token.
        let default_approved: i128 = job
            .milestones
            .iter()
            .filter(|m| m.token.is_none() && m.status == MilestoneStatus::Approved)
            .map(|m| m.amount)
            .sum();
        let default_refund = job.total_amount - default_approved;
        let default_tc = token::Client::new(&env, &job.token);
        if default_approved > 0 {
            default_tc.transfer(&env.current_contract_address(), &job.freelancer, &default_approved);
        }
        if default_refund > 0 {
            default_tc.transfer(&env.current_contract_address(), &client, &default_refund);
        }

        // Non-default tokens.
        let tb_len = job.token_balances.len();
        for idx in 0..tb_len {
            let tb = job.token_balances.get(idx).unwrap();
            let tb_approved: i128 = job
                .milestones
                .iter()
                .filter(|m| m.token.as_ref() == Some(&tb.token) && m.status == MilestoneStatus::Approved)
                .map(|m| m.amount)
                .sum();
            let tb_refund = tb.total_amount - tb_approved;
            let tc = token::Client::new(&env, &tb.token);
            if tb_approved > 0 {
                tc.transfer(&env.current_contract_address(), &job.freelancer, &tb_approved);
            }
            if tb_refund > 0 {
                tc.transfer(&env.current_contract_address(), &client, &tb_refund);
            }
        }
        let refund = default_refund; // used in event below

        // Cancel any active sub-assignments for milestones that were not approved.
        // Unapproved milestones are refunded to the client, so sub-freelancers
        // receive no payout; marking Cancelled keeps storage state consistent.
        let ms_len = job.milestones.len();
        for idx in 0..ms_len {
            let ms = job.milestones.get(idx).unwrap();
            if ms.status != MilestoneStatus::Approved {
                let sub_key = DataKey::SubAssignment(job_id, ms.id);
                let sub_opt: Option<SubAssignment> = env.storage().persistent().get(&sub_key);
                if let Some(mut sub) = sub_opt {
                    if sub.status == SubAssignmentStatus::Active {
                        let sub_fl = sub.sub_freelancer.clone();
                        sub.status = SubAssignmentStatus::Cancelled;
                        env.storage().persistent().set(&sub_key, &sub);
                        env.events().publish(
                            (symbol_short!("escrow"), symbol_short!("sub_cncl")),
                            (job_id, ms.id, sub_fl),
                        );
                    }
                }
            }
        }

        job.status = JobStatus::Cancelled;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit JobCancelled event with job_id, client address, and refund_amount.
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("cancelled")),
            (job_id, client, job.freelancer, refund),
        );

        Ok(())
    }

    /// Claim a refund for an abandoned job past the deadline + grace period.
    /// Only the client can call this. Refund excludes amounts for already-approved milestones.
    /// Fails if the freelancer has a pending (submitted) milestone awaiting approval.
    pub fn claim_refund(env: Env, job_id: u64, client: Address, nonce: u64) -> Result<(), EscrowError> {
        consume_nonce(&env, &client, &Symbol::new(&env, "claim_ref"), nonce)?;
        bump_escrow_ttl(&env, job_id);
        client.require_auth();
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.client != client {
            return Err(EscrowError::Unauthorized);
        }

        // STATE VALIDATION: Only allow refund for Funded or InProgress jobs
        require_state_funded_or_in_progress(&job)?;

        // Ensure the grace period after deadline has elapsed
        let refund_eligible_at = job.job_deadline + job.auto_refund_after;
        if env.ledger().timestamp() < refund_eligible_at {
            return Err(EscrowError::GracePeriodNotMet);
        }

        // Prevent refund if freelancer has an active pending milestone submission
        let has_pending = job
            .milestones
            .iter()
            .any(|m| m.status == MilestoneStatus::Submitted);
        if has_pending {
            return Err(EscrowError::HasPendingMilestone);
        }

        // Calculate refund: total minus already-approved milestone amounts (default token).
        let default_approved: i128 = job
            .milestones
            .iter()
            .filter(|m| m.token.is_none() && m.status == MilestoneStatus::Approved)
            .map(|m| m.amount)
            .sum();
        let refund = job.total_amount - default_approved;
        let default_tc = token::Client::new(&env, &job.token);

        // Check if there is anything at all to distribute before touching token state.
        let has_non_default_funds = job.token_balances.iter().any(|tb| tb.total_amount > 0);
        if refund <= 0 && default_approved == 0 && !has_non_default_funds {
            return Err(EscrowError::NoRefundDue);
        }

        if default_approved > 0 {
            default_tc.transfer(&env.current_contract_address(), &job.freelancer, &default_approved);
        }

        if refund <= 0 && !has_non_default_funds {
            // approved_amount paid above — no refund due to client, just update status
            job.status = JobStatus::Cancelled;
            env.storage().persistent().set(&get_job_key(job_id), &job);
            bump_job_ttl(&env, job_id);
            env.events().publish(
                (symbol_short!("escrow"), symbol_short!("refund")),
                (job_id, 0_i128, client, job.freelancer),
            );
            return Ok(());
        }

        // Transfer default-token refund to client.
        if refund > 0 {
            default_tc.transfer(&env.current_contract_address(), &client, &refund);
        }

        // Non-default tokens: pay approved amounts to freelancer, remainder to client.
        let tb_len = job.token_balances.len();
        for idx in 0..tb_len {
            let tb = job.token_balances.get(idx).unwrap();
            let tb_approved: i128 = job
                .milestones
                .iter()
                .filter(|m| m.token.as_ref() == Some(&tb.token) && m.status == MilestoneStatus::Approved)
                .map(|m| m.amount)
                .sum();
            let tb_refund = tb.total_amount - tb_approved;
            let tc = token::Client::new(&env, &tb.token);
            if tb_approved > 0 {
                tc.transfer(&env.current_contract_address(), &job.freelancer, &tb_approved);
            }
            if tb_refund > 0 {
                tc.transfer(&env.current_contract_address(), &client, &tb_refund);
            }
        }

        job.status = JobStatus::Cancelled;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("refund")),
            (job_id, refund, client, job.freelancer),
        );

        Ok(())
    }

    /// Allows an authorized admin (signer) to issue a partial refund to the client
    /// from the remaining escrow balance of a job.
    ///
    /// The refund amount is deducted from the job's `total_amount` and `funded_amount`
    /// so that the remaining milestones accurately reflect the updated escrow balance.
    /// This is useful for dispute resolution or escrow adjustments without cancelling the job.
    ///
    /// # Authorization
    /// Only registered multi-sig signers may call this function.
    ///
    /// # Errors
    /// * `NotAdmin`           — caller is not a registered signer
    /// * `JobNotFound`        — job does not exist
    /// * `InvalidStatus`      — job is in a terminal state (Completed, Cancelled, Expired, Created)
    /// * `InsufficientFunds`  — amount <= 0 or amount > remaining locked balance
    pub fn partial_refund(
        env: Env,
        caller: Address,
        job_id: u64,
        amount: i128,
        nonce: u64,
    ) -> Result<(), EscrowError> {
        consume_nonce(&env, &caller, &Symbol::new(&env, "partial_ref"), nonce)?;
        bump_escrow_ttl(&env, job_id);
        caller.require_auth();
        require_not_paused(&env)?;

        if !is_signer(&env, &caller) {
            return Err(EscrowError::NotAdmin);
        }

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // Only allow refund on jobs that actually hold escrowed funds.
        if job.status != JobStatus::Funded
            && job.status != JobStatus::InProgress
            && job.status != JobStatus::Disputed
        {
            return Err(EscrowError::InvalidStatus);
        }

        // Calculate remaining locked balance (total minus already-approved milestones).
        let approved_amount: i128 = job
            .milestones
            .iter()
            .filter(|m| m.status == MilestoneStatus::Approved)
            .map(|m| m.amount)
            .sum();
        let remaining = job.total_amount - approved_amount;

        if amount <= 0 || amount > remaining {
            return Err(EscrowError::InsufficientFunds);
        }

        // Transfer refund to the client.
        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&env.current_contract_address(), &job.client, &amount);

        // Update escrow state to reflect the refund.
        job.total_amount = job
            .total_amount
            .checked_sub(amount)
            .ok_or(EscrowError::InsufficientFunds)?;
        job.funded_amount = job.funded_amount.saturating_sub(amount);

        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit PartialRefund event.
        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "partial_ref")),
            (
                job_id,
                caller,
                job.client.clone(),
                amount,
                job.total_amount,
            ),
        );

        Ok(())
    }

    // ============================================================
    // JOB REVISION AND SCOPE RENEGOTIATION
    // ============================================================
    // These functions implement a formal proposal flow for revising
    // job milestones and budget after a job has been funded.
    //
    // Flow:
    //   Either party → propose_revision()  → stores Pending proposal
    //   Other party  → accept_revision()   → updates job + adjusts escrow
    //   Other party  → reject_revision()   → cancels proposal, no changes
    //   Proposer     → cancel_revision_proposal() → withdraws own Pending proposal
    //
    // Security invariants:
    //   - Proposer cannot accept or reject their own proposal
    //   - Proposer can cancel (withdraw) their own Pending proposal
    //   - Only one Pending proposal per job at any time
    //   - All token movements use checked arithmetic
    //   - Escrow balance always reflects the current agreed total
    // ============================================================

    /// Proposes a revision to the milestones and total budget of an active job.
    ///
    /// # Authorization
    /// Callable by either the job's client or the job's freelancer.
    /// The caller must authenticate via `caller.require_auth()`.
    ///
    /// # Arguments
    /// * `caller` — The address proposing the revision (must be client or freelancer)
    /// * `job_id` — The unique identifier of the job to revise
    /// * `new_milestones` — The proposed replacement milestone set (must be non-empty)
    ///
    /// # Behavior
    /// - Computes `new_total` as the sum of all amounts in `new_milestones`
    /// - Stores the proposal under `DataKey::RevisionProposal(job_id)`
    /// - Only one Pending proposal may exist per job — fails if one already exists
    /// - Does not modify the job's existing milestones or total until acceptance
    ///
    /// # Errors
    /// * `JobNotFound` — if the job does not exist (use existing error variant)
    /// * `NotAuthorizedForProposalAction` — if caller is neither client nor freelancer
    /// * `RevisionProposalAlreadyExists` — if a Pending proposal already exists
    /// * `EmptyMilestonesProposed` — if new_milestones is empty
    /// * `ProposalTotalMismatch` — if sum of milestone amounts does not equal computed new_total
    /// * `InvalidStatus` — if the job is Disputed or in any terminal state (Completed,
    ///   Cancelled, Expired) — the revision flow assumes the job is still actively in
    ///   progress with nothing finalized
    pub fn propose_revision(
        env: Env,
        caller: Address,
        job_id: u64,
        new_milestones: Vec<Milestone>,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        caller.require_auth();

        // 1. Load the job
        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // 2. Verify caller is a party to this job
        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }

        // Revision proposals are not supported for multi-token jobs.
        if !job.token_balances.is_empty() {
            return Err(EscrowError::InvalidStatus);
        }

        // Freeze revisions while a dispute is active so later dispute
        // resolution still operates on the original milestone set and total, and
        // reject on any terminal state (Completed, Cancelled, Expired) — the whole
        // revision flow assumes the job is still actively in progress and nothing
        // has been finalized yet.
        require_state_not_terminal(&job)?;
        require_state_not_disputed(&job)?;

        // Reject proposals that would orphan an active sub-assignment.
        // A sub-assignment is keyed by milestone id; if the proposal drops or renumbers
        // a milestone that carries an active sub-assignment, the sub-freelancer would have
        // no remaining payout path after acceptance.
        for old_ms in job.milestones.iter() {
            let sub_key = DataKey::SubAssignment(job_id, old_ms.id);
            if let Some(sub) = env
                .storage()
                .persistent()
                .get::<DataKey, SubAssignment>(&sub_key)
            {
                if sub.status == SubAssignmentStatus::Active {
                    let retained = new_milestones.iter().any(|m| m.id == old_ms.id);
                    if !retained {
                        return Err(EscrowError::InvalidStatus);
                    }
                }
            }
        }

        // 3. Assert no existing Pending proposal, allowing overwrite of expired ones
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
        {
            if existing.status == ProposalStatus::Pending {
                let expiry_secs: u64 = env
                    .storage()
                    .instance()
                    .get(&DataKey::ProposalExpiry)
                    .unwrap_or(DEFAULT_PROPOSAL_EXPIRY_SECS);
                let now = env.ledger().timestamp();
                if now < existing.created_at + expiry_secs {
                    return Err(EscrowError::RevisionProposalAlreadyExists);
                }
                // Expired proposal — fall through to overwrite with new one
            }
        }

        // 4. Validate milestones
        if new_milestones.is_empty() {
            return Err(EscrowError::EmptyMilestonesProposed);
        }
        if new_milestones.len() > MAX_MILESTONES {
            return Err(EscrowError::TooManyMilestones);
        }

        let current_timestamp = env.ledger().timestamp();
        let mut prev_deadline: u64 = 0;
        for (i, milestone) in new_milestones.iter().enumerate() {
            if milestone.amount <= 0 {
                return Err(EscrowError::InvalidMilestone);
            }
            if milestone.deadline <= current_timestamp {
                return Err(EscrowError::MilestoneDeadlineInPast);
            }
            if milestone.deadline > job.job_deadline {
                return Err(EscrowError::InvalidDeadline);
            }
            if i > 0 && milestone.deadline <= prev_deadline {
                return Err(EscrowError::MilestoneDeadlinesNotOrdered);
            }
            prev_deadline = milestone.deadline;
        }

        // 5. Compute new_total as the sum of all milestone amounts
        // Use checked arithmetic — no overflow permitted
        let new_total: i128 = new_milestones
            .iter()
            .try_fold(0i128, |acc, m| acc.checked_add(m.amount))
            .ok_or(EscrowError::ProposalTotalMismatch)?;

        if new_total <= 0 {
            return Err(EscrowError::ProposalTotalMismatch);
        }

        // 6. Construct and store the proposal
        let proposal = RevisionProposal {
            proposer: caller.clone(),
            new_milestones,
            new_total,
            status: ProposalStatus::Pending,
            created_at: env.ledger().timestamp(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::RevisionProposal(job_id), &proposal);
        // Extend TTL
        env.storage().persistent().extend_ttl(
            &DataKey::RevisionProposal(job_id),
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        // 7. Emit event
        env.events().publish(
            (Symbol::new(&env, "revision_proposed"),),
            (job_id, caller, job.client, job.freelancer, new_total),
        );

        Ok(())
    }

    /// Accepts a pending revision proposal, updating the job's milestones and adjusting escrow.
    ///
    /// # Authorization
    /// Callable ONLY by the party who did NOT propose the revision.
    /// The proposer cannot accept their own proposal.
    ///
    /// # Arguments
    /// * `caller` — The non-proposing party (client or freelancer)
    /// * `job_id` — The job whose proposal is being accepted
    ///
    /// # Behavior
    /// The top-up/refund transfer is computed against `job.funded_amount` — the actual
    /// escrowed balance — rather than the nominal `job.total_amount`, so it always
    /// reconciles the real contract balance to `new_total` regardless of prior
    /// disbursements or funding drift:
    ///
    /// ## If new_total > funded_amount (budget increase):
    ///   - The difference is required from the client as a top-up
    ///   - Caller (if client) must have pre-authorized the token transfer
    ///   - The contract transfers (new_total - funded_amount) from client to itself
    ///
    /// ## If new_total < funded_amount (budget decrease):
    ///   - The difference is refunded to the client immediately
    ///   - The contract transfers (funded_amount - new_total) from itself to client
    ///
    /// ## If new_total == funded_amount (no budget change):
    ///   - Only milestone structure changes — no token movement occurs
    ///
    /// A revision can never reduce `new_total` below funds already disbursed to the
    /// freelancer (see `RevisionBelowDisbursedAmount` below), which guarantees the
    /// refund path never tries to claw back more than the job's real remaining balance.
    ///
    /// ## Revision History:
    ///   - Before overwriting, the current milestone structure is snapshotted
    ///   - The snapshot is appended to an immutable revision history for audit trail
    ///
    /// # Errors
    /// * `RevisionProposalNotFound` — if no proposal exists for this job
    /// * `ProposalNotPending` — if the proposal is not in Pending status
    /// * `NotAuthorizedForProposalAction` — if caller is the proposer or not a party
    /// * `InvalidStatus` — if the job is Disputed or in a terminal state (Completed,
    ///   Cancelled, Expired), which can happen if the job changed state after the
    ///   proposal was created but before it was accepted
    /// * `RevisionBelowDisbursedAmount` — if the proposal drops a milestone that already
    ///   has disbursed funds, or values it below what's already been paid out
    /// * `InsufficientTopUp` — if new_total > funded_amount and top-up transfer fails
    pub fn accept_revision(env: Env, caller: Address, job_id: u64) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        caller.require_auth();

        // 1. Load job
        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // 2. Load proposal — must exist and be Pending
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
            .ok_or(EscrowError::RevisionProposalNotFound)?;

        if proposal.status != ProposalStatus::Pending {
            return Err(EscrowError::ProposalNotPending);
        }

        // 3. Verify caller is a party and is NOT the proposer
        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }
        if caller == proposal.proposer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }

        // Revision proposals are not supported for multi-token jobs.
        if !job.token_balances.is_empty() {
            return Err(EscrowError::InvalidStatus);
        }

        // The job may have moved into a dispute or a terminal state after the proposal
        // was created but before it was accepted — re-check here, right before mutating
        // anything, rather than trusting the state that held at propose time.
        require_state_not_terminal(&job)?;
        require_state_not_disputed(&job)?;

        // Guard against a revision silently erasing money already paid to the freelancer:
        // for every existing milestone that has disbursed funds (via release_partial_payment
        // or release_milestone), the proposal must still carry that milestone id with a
        // value at least equal to what's already been paid out.
        for old_milestone in job.milestones.iter() {
            let disbursed = get_milestone_disbursed(&env, job_id, old_milestone.id);
            if disbursed <= 0 {
                continue;
            }
            let still_covered = proposal
                .new_milestones
                .iter()
                .any(|m| m.id == old_milestone.id && m.amount >= disbursed);
            if !still_covered {
                return Err(EscrowError::RevisionBelowDisbursedAmount);
            }
        }

        // Sub-assignment reconciliation: for every old milestone that carries an active
        // sub-assignment, (a) block proposals that drop the milestone id entirely — the
        // sub-freelancer would have no remaining payout path — and (b) proportionally
        // scale the sub-amount when the milestone budget shrinks, so neither party is
        // left with a disproportionate zero-payout. This is a safety net even when
        // propose_revision already caught the orphan case, because a sub-assignment
        // could be created in the window between propose and accept.
        for old_ms in job.milestones.iter() {
            let sub_key = DataKey::SubAssignment(job_id, old_ms.id);
            let sub_opt: Option<SubAssignment> = env.storage().persistent().get(&sub_key);
            if let Some(mut sub) = sub_opt {
                if sub.status != SubAssignmentStatus::Active {
                    continue;
                }
                let new_ms_opt = proposal.new_milestones.iter().find(|m| m.id == old_ms.id);
                match new_ms_opt {
                    None => {
                        return Err(EscrowError::InvalidStatus);
                    }
                    Some(new_ms) if new_ms.amount < sub.amount => {
                        // Scale proportionally: new_sub = sub.amount * new_ms.amount / old_ms.amount.
                        // old_ms.amount > 0 is guaranteed by milestone validation at creation/propose time.
                        let new_sub_amount = (sub.amount * new_ms.amount) / old_ms.amount;
                        sub.amount = new_sub_amount;
                        env.storage().persistent().set(&sub_key, &sub);
                        env.storage().persistent().extend_ttl(
                            &sub_key,
                            TTL_THRESHOLD_LEDGERS,
                            TTL_EXTEND_TO_LEDGERS,
                        );
                    }
                    _ => {}
                }
            }
        }

        // 4. Snapshot current milestones to revision history BEFORE overwriting
        let mut history: Vec<MilestoneRevision> = env
            .storage()
            .persistent()
            .get(&DataKey::RevisionHistory(job_id))
            .unwrap_or(Vec::new(&env));

        let revision_index = history.len();
        let snapshot = MilestoneRevision {
            revision_index: revision_index as u32,
            milestones: job.milestones.clone(),
            total_amount: job.total_amount,
            revised_at: env.ledger().timestamp(),
            revised_by: caller.clone(),
        };
        history.push_back(snapshot);

        // Store updated history
        env.storage()
            .persistent()
            .set(&DataKey::RevisionHistory(job_id), &history);
        env.storage().persistent().extend_ttl(
            &DataKey::RevisionHistory(job_id),
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        // 5. Compute balance delta from the job's actual funded (escrowed) balance —
        // not from the nominal `total_amount` — so the transfer always reconciles the
        // real contract balance to the new agreed total, even if `funded_amount` had
        // drifted from `total_amount` (e.g. a job funded incrementally via
        // `top_up_escrow`). This is what actually moves tokens below, so it must be
        // grounded in what's really sitting in escrow rather than a stale nominal figure.
        let new_total = proposal.new_total;
        let delta = new_total - job.funded_amount; // positive = increase, negative = decrease, zero = unchanged

        // 6. Handle escrow balance adjustment
        let token_client = token::Client::new(&env, &job.token);

        if delta > 0 {
            // Budget increased — require client to top up the difference
            token_client.transfer(
                &job.client,                     // from: client
                &env.current_contract_address(), // to: this contract
                &delta,
            );
            job.funded_amount = job
                .funded_amount
                .checked_add(delta)
                .ok_or(EscrowError::InsufficientTopUp)?;
        } else if delta < 0 {
            // Budget decreased — refund the absolute difference to client
            let refund_amount = delta.checked_abs().ok_or(EscrowError::InsufficientTopUp)?;
            token_client.transfer(
                &env.current_contract_address(), // from: this contract
                &job.client,                     // to: client
                &refund_amount,
            );
            job.funded_amount = job.funded_amount.saturating_sub(refund_amount);
        }
        // delta == 0: no token movement needed

        // 7. Update job milestones and total. For any milestone id that already carries
        // disbursed funds, rebase the incoming entry onto the real remaining balance
        // (new nominal value minus what's already been paid) instead of trusting the
        // proposer's raw entry verbatim — otherwise a later release could pay out the
        // full new amount on top of funds already sent to the freelancer.
        let mut final_milestones: Vec<Milestone> = Vec::new(&env);
        for new_milestone in proposal.new_milestones.iter() {
            let was_existing = job.milestones.iter().any(|old| old.id == new_milestone.id);
            let disbursed = if was_existing {
                get_milestone_disbursed(&env, job_id, new_milestone.id)
            } else {
                0
            };
            if disbursed > 0 {
                let remaining = new_milestone.amount - disbursed; // >= 0, enforced above
                let (status, amount) = if remaining <= 0 {
                    (MilestoneStatus::Approved, 0)
                } else {
                    (MilestoneStatus::PartiallyPaid, remaining)
                };
                final_milestones.push_back(Milestone {
                    id: new_milestone.id,
                    description: new_milestone.description.clone(),
                    amount,
                    status,
                    deadline: new_milestone.deadline,
                    token: new_milestone.token.clone(),
                });
            } else {
                final_milestones.push_back(new_milestone.clone());
            }
        }
        job.milestones = final_milestones;
        job.total_amount = new_total;

        // 8. Persist updated job
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // 9. Update proposal status to Accepted
        proposal.status = ProposalStatus::Accepted;
        env.storage()
            .persistent()
            .set(&DataKey::RevisionProposal(job_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::RevisionProposal(job_id),
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        // 10. Emit event
        env.events().publish(
            (Symbol::new(&env, "revision_accepted"),),
            (job_id, caller, job.client, job.freelancer, new_total, delta),
        );

        Ok(())
    }

    /// Rejects a pending revision proposal. No changes are made to the job or escrow.
    ///
    /// # Authorization
    /// Callable ONLY by the party who did NOT propose the revision.
    /// The proposer cannot reject their own proposal.
    ///
    /// # Arguments
    /// * `caller` — The non-proposing party
    /// * `job_id` — The job whose proposal is being rejected
    ///
    /// # Behavior
    /// - Sets proposal status to Rejected
    /// - Job milestones, total, and escrow balance remain completely unchanged
    /// - After rejection, a new proposal may be submitted by either party
    ///
    /// # Errors
    /// * `RevisionProposalNotFound` — if no proposal exists
    /// * `ProposalNotPending` — if the proposal is not Pending
    /// * `NotAuthorizedForProposalAction` — if caller is the proposer or not a party
    pub fn reject_revision(env: Env, caller: Address, job_id: u64) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        caller.require_auth();

        // 1. Load job
        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // 2. Load and validate proposal
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
            .ok_or(EscrowError::RevisionProposalNotFound)?;

        if proposal.status != ProposalStatus::Pending {
            return Err(EscrowError::ProposalNotPending);
        }

        // 3. Verify caller is a party and NOT the proposer
        if caller != job.client && caller != job.freelancer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }
        if caller == proposal.proposer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }

        // 4. Mark proposal as Rejected — job and escrow unchanged
        proposal.status = ProposalStatus::Rejected;
        env.storage()
            .persistent()
            .set(&DataKey::RevisionProposal(job_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::RevisionProposal(job_id),
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        // 5. Emit event
        env.events().publish(
            (Symbol::new(&env, "revision_rejected"),),
            (job_id, caller, job.client, job.freelancer),
        );

        Ok(())
    }

    /// Cancels a pending revision proposal. Only the original proposer may cancel.
    ///
    /// # Authorization
    /// Callable ONLY by the party who originally proposed the revision.
    ///
    /// # Arguments
    /// * `caller` — The original proposer
    /// * `job_id` — The job whose proposal is being cancelled
    ///
    /// # Behavior
    /// - Removes the proposal from storage entirely
    /// - Job milestones, total, and escrow balance remain completely unchanged
    /// - After cancellation, a new proposal may be submitted by either party
    ///
    /// # Errors
    /// * `RevisionProposalNotFound` — if no proposal exists
    /// * `ProposalNotPending` — if the proposal is not Pending
    /// * `NotAuthorizedForProposalAction` — if caller is not the original proposer
    pub fn cancel_revision_proposal(
        env: Env,
        caller: Address,
        job_id: u64,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;
        caller.require_auth();

        // 1. Load job
        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // 2. Load and validate proposal
        let proposal = env
            .storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
            .ok_or(EscrowError::RevisionProposalNotFound)?;

        if proposal.status != ProposalStatus::Pending {
            return Err(EscrowError::ProposalNotPending);
        }

        // 3. Verify caller is the original proposer
        if caller != proposal.proposer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }

        // 4. Remove the proposal from storage — slot is immediately available
        env.storage()
            .persistent()
            .remove(&DataKey::RevisionProposal(job_id));

        // 5. Emit event
        env.events().publish(
            (Symbol::new(&env, "revision_cancelled"),),
            (job_id, caller, job.client, job.freelancer),
        );

        Ok(())
    }

    /// Allows the original proposer to explicitly expire their own pending revision proposal
    /// after the configured proposal TTL has elapsed.
    ///
    /// # Authorization
    /// Only the original proposer may call this function.
    ///
    /// # Errors
    /// * `JobNotFound`                     — job does not exist
    /// * `RevisionProposalNotFound`        — no proposal exists for this job
    /// * `ProposalNotPending`              — proposal is not in Pending status
    /// * `NotAuthorizedForProposalAction`  — caller is not the original proposer
    /// * `ProposalNotExpirable`            — TTL has not yet elapsed
    pub fn expire_proposal(
        env: Env,
        caller: Address,
        job_id: u64,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;
        caller.require_auth();

        // Validate job exists.
        let _job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        // Load proposal.
        let mut proposal = env
            .storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
            .ok_or(EscrowError::RevisionProposalNotFound)?;

        if proposal.status != ProposalStatus::Pending {
            return Err(EscrowError::ProposalNotPending);
        }

        if caller != proposal.proposer {
            return Err(EscrowError::NotAuthorizedForProposalAction);
        }

        // Check if the proposal TTL has elapsed.
        let expiry_secs: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ProposalExpiry)
            .unwrap_or(DEFAULT_PROPOSAL_EXPIRY_SECS);
        let now = env.ledger().timestamp();
        if now < proposal.created_at + expiry_secs {
            return Err(EscrowError::ProposalNotExpirable);
        }

        // Mark proposal as Rejected.
        proposal.status = ProposalStatus::Rejected;
        env.storage()
            .persistent()
            .set(&DataKey::RevisionProposal(job_id), &proposal);
        env.storage().persistent().extend_ttl(
            &DataKey::RevisionProposal(job_id),
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        // Emit revision_expired event.
        env.events().publish(
            (Symbol::new(&env, "revision_expired"),),
            (job_id, caller, proposal.proposer),
        );

        Ok(())
    }

    /// Returns the current revision proposal for the given job, if one exists.
    /// Returns None if no proposal has been submitted or if the last proposal was resolved.
    ///
    /// # Arguments
    /// * `job_id` — The job to query
    pub fn get_revision_proposal(env: Env, job_id: u64) -> Option<RevisionProposal> {
        env.storage()
            .persistent()
            .get::<DataKey, RevisionProposal>(&DataKey::RevisionProposal(job_id))
    }

    /// Returns the complete revision history for a job as an append-only audit trail.
    ///
    /// Each entry in the history represents a snapshot of the milestone structure
    /// at the time a revision was accepted. The history is ordered chronologically,
    /// with index 0 being the oldest revision and the last entry being the most recent.
    ///
    /// # Arguments
    /// * `job_id` — The job whose revision history to retrieve
    ///
    /// # Returns
    /// A vector of `MilestoneRevision` snapshots. Returns an empty vector if no
    /// revisions have been accepted for this job.
    ///
    /// # Use Cases
    /// - Audit trail for dispute resolution
    /// - Transparency for both parties to see how scope evolved
    /// - Historical record for compliance and reporting
    pub fn get_revision_history(env: Env, job_id: u64) -> Vec<MilestoneRevision> {
        env.storage()
            .persistent()
            .get(&DataKey::RevisionHistory(job_id))
            .unwrap_or(Vec::new(&env))
    }
    /// Expire a job whose deadline has passed. Callable by anyone.
    ///
    /// Asserts that the ledger timestamp has surpassed the job's `job_deadline` and
    /// that the job has not already reached a terminal state. On success the full
    /// remaining escrowed balance is refunded to the client and the status is set
    /// to `Expired`.
    ///
    /// # Errors
    /// * `JobNotFound`       — job does not exist.
    /// * `DeadlineNotPassed` — `env.ledger().timestamp() <= job.job_deadline`.
    /// * `InvalidStatus`     — job is already `Completed`, `Cancelled`, or `Expired`.
    pub fn expire_job(env: Env, job_id: u64) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if env.ledger().timestamp() <= job.job_deadline {
            return Err(EscrowError::DeadlineNotPassed);
        }

        // STATE VALIDATION: Job must be in an expirable state (not terminal)
        require_state_expirable(&job)?;

        // Refund remaining escrowed balance and pay approved milestones to freelancer.
        // Operates per-token for multi-token jobs.
        let funded_state = job.status == JobStatus::Funded
            || job.status == JobStatus::InProgress
            || job.status == JobStatus::Disputed;

        let default_approved: i128 = job
            .milestones
            .iter()
            .filter(|m| m.token.is_none() && m.status == MilestoneStatus::Approved)
            .map(|m| m.amount)
            .sum();
        let default_refund = job.total_amount - default_approved;
        let default_tc = token::Client::new(&env, &job.token);

        if funded_state {
            if default_approved > 0 {
                default_tc.transfer(&env.current_contract_address(), &job.freelancer, &default_approved);
            }
            if default_refund > 0 {
                default_tc.transfer(&env.current_contract_address(), &job.client, &default_refund);
            }

            // Non-default tokens.
            let tb_len = job.token_balances.len();
            for idx in 0..tb_len {
                let tb = job.token_balances.get(idx).unwrap();
                let tb_approved: i128 = job
                    .milestones
                    .iter()
                    .filter(|m| {
                        m.token.as_ref() == Some(&tb.token)
                            && m.status == MilestoneStatus::Approved
                    })
                    .map(|m| m.amount)
                    .sum();
                let tb_refund = tb.total_amount - tb_approved;
                let tc = token::Client::new(&env, &tb.token);
                if tb_approved > 0 {
                    tc.transfer(&env.current_contract_address(), &job.freelancer, &tb_approved);
                }
                if tb_refund > 0 {
                    tc.transfer(&env.current_contract_address(), &job.client, &tb_refund);
                }
            }
        }
        let refund = default_refund; // used in event below

        job.status = JobStatus::Expired;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        env.events().publish(
            (symbol_short!("escrow"), Symbol::new(&env, "job_expired")),
            (job_id, job.client, job.freelancer, job.token, refund),
        );

        Ok(())
    }

    // ============================================================
    // SUB-CONTRACTING (issue #898)
    // ============================================================
    // The main freelancer can promise a portion of a milestone's payout
    // to a sub-freelancer. The promise is enforced at payout time: when
    // the client approves the milestone (and payment is released), the
    // contract automatically routes `amount` to the sub-freelancer and
    // the remainder to the main freelancer.
    //
    // Design notes:
    //   - No pre-funding: the sub-assignment is a promise, not a separate
    //     escrow. Funds only move at the moment the parent milestone pays.
    //   - At most one active sub-assignment per milestone.
    //   - If the job is cancelled/expired without the milestone being
    //     approved, the sub-assignment is marked Cancelled with no payout.
    //   - Disputes settle via the normal dispute contract; the main
    //     freelancer receives the full dispute payout (sub-assignment is
    //     not involved), isolating sub-level relationships from the parent
    //     dispute.
    // ============================================================

    /// Create a sub-assignment for a milestone, promising `amount` of the
    /// payout to `sub_freelancer` when the client approves the milestone.
    ///
    /// # Authorization
    /// Only the job's main freelancer may call this.
    ///
    /// # Errors
    /// * `JobNotFound`       — job does not exist.
    /// * `Unauthorized`      — caller is not the job's freelancer.
    /// * `InvalidStatus`     — job is not Funded/InProgress; milestone is already
    ///                         Submitted/Approved/PartiallyPaid; or an active
    ///                         sub-assignment already exists for this milestone.
    /// * `MilestoneNotFound` — milestone index is out of range.
    /// * `InvalidAmount`     — `amount` is <= 0 or exceeds the milestone's amount.
    /// * `ContractPaused`    — the contract is paused.
    pub fn sub_assign_milestone(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        freelancer: Address,
        sub_freelancer: Address,
        amount: i128,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        freelancer.require_auth();
        require_not_paused(&env)?;

        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.freelancer != freelancer {
            return Err(EscrowError::Unauthorized);
        }

        if sub_freelancer == freelancer {
            return Err(EscrowError::Unauthorized);
        }

        require_state_funded_or_in_progress(&job)?;

        let milestone = job
            .milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;

        // Sub-assignment is only meaningful while the milestone has not yet been
        // submitted for review; once submitted, the client is about to approve it.
        if milestone.status != MilestoneStatus::Pending
            && milestone.status != MilestoneStatus::InProgress
        {
            return Err(EscrowError::InvalidStatus);
        }

        if amount <= 0 || amount > milestone.amount {
            return Err(EscrowError::InvalidAmount);
        }

        let sub_key = DataKey::SubAssignment(job_id, milestone_id);
        if let Some(existing) = env
            .storage()
            .persistent()
            .get::<DataKey, SubAssignment>(&sub_key)
        {
            if existing.status == SubAssignmentStatus::Active {
                // Reuses InvalidStatus: the milestone already has an active assignment.
                return Err(EscrowError::InvalidStatus);
            }
        }

        let sub = SubAssignment {
            job_id,
            milestone_id,
            sub_freelancer: sub_freelancer.clone(),
            amount,
            status: SubAssignmentStatus::Active,
        };
        env.storage().persistent().set(&sub_key, &sub);
        env.storage().persistent().extend_ttl(
            &sub_key,
            TTL_THRESHOLD_LEDGERS,
            TTL_EXTEND_TO_LEDGERS,
        );

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("sub_asgn")),
            (job_id, milestone_id, freelancer, sub_freelancer, amount),
        );

        Ok(())
    }

    /// Cancel an active sub-assignment before the milestone has been submitted.
    ///
    /// Only the main freelancer may cancel. Cancellation is blocked once the
    /// milestone is in Submitted state (the client is about to release payment).
    ///
    /// # Errors
    /// * `JobNotFound`       — job does not exist.
    /// * `Unauthorized`      — caller is not the job's freelancer.
    /// * `InvalidStatus`     — no active sub-assignment exists for this milestone,
    ///                         or the milestone is already Submitted/Approved.
    /// * `ContractPaused`    — the contract is paused.
    pub fn cancel_sub_assignment(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        freelancer: Address,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        freelancer.require_auth();
        require_not_paused(&env)?;

        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);

        if job.freelancer != freelancer {
            return Err(EscrowError::Unauthorized);
        }

        let milestone = job
            .milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;

        if milestone.status == MilestoneStatus::Submitted
            || milestone.status == MilestoneStatus::PartiallyPaid
            || milestone.status == MilestoneStatus::Approved
        {
            return Err(EscrowError::InvalidStatus);
        }

        let sub_key = DataKey::SubAssignment(job_id, milestone_id);
        let mut sub: SubAssignment = env
            .storage()
            .persistent()
            .get(&sub_key)
            // Reuses InvalidStatus: no active sub-assignment to cancel.
            .ok_or(EscrowError::InvalidStatus)?;

        if sub.status != SubAssignmentStatus::Active {
            return Err(EscrowError::InvalidStatus);
        }

        let sub_fl = sub.sub_freelancer.clone();
        sub.status = SubAssignmentStatus::Cancelled;
        env.storage().persistent().set(&sub_key, &sub);

        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("sub_cncl")),
            (job_id, milestone_id, sub_fl),
        );

        Ok(())
    }

    /// Returns the sub-assignment for a given (job_id, milestone_id), if one exists.
    pub fn get_sub_assignment(env: Env, job_id: u64, milestone_id: u32) -> Option<SubAssignment> {
        bump_escrow_ttl(&env, job_id);
        env.storage()
            .persistent()
            .get(&DataKey::SubAssignment(job_id, milestone_id))
    }

    /// Get job details by ID.
    pub fn get_job(env: Env, job_id: u64) -> Result<Job, EscrowError> {
        bump_escrow_ttl(&env, job_id);
        let job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;
        bump_job_ttl(&env, job_id);
        Ok(job)
    }

    /// Get total number of jobs.
    pub fn get_job_count(env: Env) -> u64 {
        let count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::JobCount)
            .unwrap_or(0);
        bump_job_count_ttl(&env);
        count
    }

    /// Check if a milestone is overdue.
    pub fn is_milestone_overdue(env: Env, job_id: u64, milestone_id: u32) -> bool {
        bump_escrow_ttl(&env, job_id);
        if let Some(job) = env
            .storage()
            .persistent()
            .get::<_, Job>(&get_job_key(job_id))
        {
            if let Some(milestone) = job.milestones.get(milestone_id) {
                return env.ledger().timestamp() > milestone.deadline;
            }
        }
        false
    }

    /// Extend the deadline for a milestone (requires mutual agreement).
    pub fn extend_deadline(
        env: Env,
        job_id: u64,
        milestone_id: u32,
        new_deadline: u64,
    ) -> Result<(), EscrowError> {
        bump_escrow_ttl(&env, job_id);
        require_not_paused(&env)?;

        let mut job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(job_id))
            .ok_or(EscrowError::JobNotFound)?;

        job.client.require_auth();
        job.freelancer.require_auth();

        // Reject calls on jobs that have already reached a terminal state
        // (Completed, Cancelled, or Expired).  These are the same guard used by
        // propose_revision and submit_milestone.
        require_state_not_terminal(&job)?;
        // Reject calls while a dispute is active — arbitrators and the other
        // party rely on deadline state staying fixed during resolution.
        require_state_not_disputed(&job)?;

        let mut milestones = job.milestones.clone();
        let mut milestone = milestones
            .get(milestone_id)
            .ok_or(EscrowError::MilestoneNotFound)?;

        if new_deadline <= env.ledger().timestamp() {
            return Err(EscrowError::InvalidDeadline);
        }

        // A deadline extension must move the milestone forward in time rather than
        // shrinking or no-oping it.
        if new_deadline <= milestone.deadline {
            return Err(EscrowError::InvalidDeadline);
        }

        // If the business rule requires it, this can be re-enabled to prevent
        // milestone deadlines from moving beyond the job-level deadline.
        // if new_deadline > job.job_deadline {
        //     return Err(EscrowError::InvalidDeadline);
        // }

        milestone.deadline = new_deadline;
        milestones.set(milestone_id, milestone);

        job.milestones = milestones;
        env.storage().persistent().set(&get_job_key(job_id), &job);
        bump_job_ttl(&env, job_id);

        // Emit deadline extension event
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("deadline")),
            (job_id, milestone_id, new_deadline),
        );

        Ok(())
    }

    /// Permissionless function to extend the TTL of an escrow's persistent storage.
    ///
    /// This function allows anyone to extend the TTL of an escrow's storage to prevent
    /// it from being archived by Soroban's rent mechanism. This is particularly useful
    /// for long-running escrows that may approach their TTL expiry.
    ///
    /// # Arguments
    /// * `escrow_id` — The unique identifier of the escrow (job) to bump
    ///
    /// # Behavior
    /// - Verifies the escrow exists by reading it from persistent storage
    /// - Extends the TTL of the escrow's storage key to TTL_EXTEND_TO_LEDGERS (~30 days)
    /// - Can be called for escrows in any state, including terminal states (Completed, Cancelled, Expired)
    /// - Does not modify any escrow state or emit events
    ///
    /// # Errors
    /// * `JobNotFound` — if no escrow exists with the given ID
    ///
    /// # Notes
    /// - This function is intentionally permissionless to allow anyone to maintain storage
    /// - Terminal escrows may still need TTL extension for historical/audit purposes
    /// - Only extends the main job storage key; ephemeral keys (proposals, timestamps) are
    ///   extended by their respective functions when accessed
    pub fn bump_escrow(env: Env, escrow_id: u64) -> Result<(), EscrowError> {
        // Verify the escrow exists
        let _job: Job = env
            .storage()
            .persistent()
            .get(&get_job_key(escrow_id))
            .ok_or(EscrowError::JobNotFound)?;

        // Extend TTL for the escrow storage key
        bump_job_ttl(&env, escrow_id);

        Ok(())
    }

    /// Extend the TTL of an active escrow.
    /// Returns JobNotFound if the job is not found or archived.
    pub fn extend_escrow_ttl(env: Env, job_id: u64) -> Result<(), EscrowError> {
        let key = DataKey::Job(job_id);
        if !env.storage().persistent().has(&key) {
            return Err(EscrowError::JobNotFound);
        }
        bump_escrow_ttl(&env, job_id);

        let new_expiry_ledger = env.ledger().sequence() + ESCROW_TTL_LEDGERS;
        env.events().publish(
            (symbol_short!("escrow"), symbol_short!("ttl_ext")),
            TtlExtendedEvent {
                job_id,
                new_expiry_ledger,
            },
        );
        Ok(())
    }

    /// Restore an archived escrow entry.
    /// Bumps the TTL to keep the escrow active.
    pub fn restore_escrow(env: Env, job_id: u64) -> Result<(), EscrowError> {
        let key = DataKey::Job(job_id);

        if !env.storage().persistent().has(&key) {
            return Err(EscrowError::JobNotFound);
        }

        bump_escrow_ttl(&env, job_id);
        Ok(())
    }
}

/// Reputation-weighted on-chain governance for protocol parameters (issue #899).
mod governance;

#[cfg(test)]
mod test;

#[cfg(test)]
mod fuzz;

#[cfg(test)]
mod governance_test;
