#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, vec, Address, BytesN, Env,
    IntoVal, String, Symbol, Vec,
};

// Import reputation contract types for cross-contract calls
mod reputation {
    use soroban_sdk::{contracttype, Address};

    #[contracttype]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct UserReputation {
        pub user: Address,
        pub total_score: u64,
        pub total_weight: u64,
        pub review_count: u32,
        pub last_updated_ts: u32,
    }
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DisputeError {
    DisputeNotFound = 1,
    Unauthorized = 2,
    AlreadyVoted = 3,
    VotingClosed = 4,
    NotEnoughVotes = 5,
    InvalidParty = 6,
    AlreadyResolved = 7,
    InsufficientReputation = 8,
    NotInitialized = 9,
    ConflictOfInterest = 10,
    ContractPaused = 11,
    NotAdmin = 12,
    DisputeCooldown = 13,
    VotingPeriodNotExpired = 14,
    DelegationNotFound = 15,
    AlreadyDelegated = 16,
    DelegateAlreadyVoted = 17,
    InvalidSplitBps = 18,
    AppealWindowExpired = 19,
    AlreadyAppealed = 20,
    AppealNotFound = 21,
    NonceReplay = 22,
    DuplicateArbitrator = 23,
    InvalidArbitrator = 24,
    ExclusionNotConfirmed = 25,
    ReplacementUnavailable = 26,
    InsufficientActiveArbitrators = 27,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisputeStatus {
    Open,
    Voting,
    ResolvedForClient,
    ResolvedForFreelancer,
    RefundedBoth,
    RefundSplit(u32),
    SplitAward(u32),
    Escalated,
    /// Filing was determined to be in bad faith by a 4/5 supermajority of arbitrators.
    MaliciousDisputeFiling,
    /// Escrow callback failed; the intended resolution is cached and can be retried.
    ResolutionFailed,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TieBreakMethod {
    FavorFreelancer,
    FavorClient,
    Escalate,
    RefundBoth,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisputeResolution {
    ClientWins,
    FreelancerWins,
    RefundBoth,
    RefundSplit(u32),
    SplitAward(u32),
    Escalate,
    /// Dispute was filed in bad faith; initiator stake is slashed to treasury and reputation penalised.
    MaliciousFiling,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppealStatus {
    Open,
    Voting,
    ResolvedForClient,
    ResolvedForFreelancer,
    RefundedBoth,
    RefundSplit(u32),
    Escalated,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appeal {
    pub id: u64,
    pub dispute_id: u64,
    pub appellant: Address,
    pub excluded_arbitrators: Vec<Address>,
    pub status: AppealStatus,
    pub votes_for_client: u32,
    pub votes_for_freelancer: u32,
    pub votes_for_refund_split: u32,
    pub refund_split_sum: u64,
    pub created_at: u64,
    pub voting_deadline: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VoteChoice {
    Client,
    Freelancer,
    RefundSplit(u32),
    /// Vote that the dispute initiator filed in bad faith.
    MaliciousFiling,
    /// Vote to split the award proportionally; client_bps + freelancer_bps must equal 10_000.
    SplitAward(u32, u32),
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Vote {
    pub voter: Address,
    pub choice: VoteChoice,
    pub reason: String,
    pub timestamp: u64,
}

/// A single piece of evidence attached to a dispute.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvidenceRecord {
    pub submitted_by: Address,
    pub evidence_hash: BytesN<32>,
    pub ledger: u32,
}

/// Maximum number of arbitrators that can be assigned to a single dispute.
/// This limit ensures O(1) resolution complexity and prevents instruction limit exceeded errors.
pub const MAX_ARBITRATORS: u32 = 7;

/// Number of arbitrators randomly assigned to each dispute.
/// Must be <= MAX_ARBITRATORS.
pub const ARBITRATORS_PER_DISPUTE: u32 = 5;

/// Number of votes for a single outcome that triggers automatic resolution.
pub const AUTO_RESOLVE_VOTE_THRESHOLD: u32 = 3;

/// Maximum number of evidence records that can be submitted per dispute.
/// This prevents unbounded storage growth and excessive TTL-extension costs.
pub const MAX_EVIDENCE_PER_DISPUTE: u32 = 50;

/// Incremental tally accumulator for O(1) vote counting and verdict finalization.
/// Instead of iterating over all votes during resolution, we maintain running totals
/// that are updated in O(1) time during each `cast_vote` operation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisputeTally {
    /// Total weight of votes cast for the client.
    pub client_weight: u64,
    /// Total weight of votes cast for the freelancer.
    pub freelancer_weight: u64,
    /// Total weight of all votes cast (sum of all vote weights).
    pub total_weight_cast: u64,
    /// Number of votes cast (equal to number of arbitrators who have voted).
    pub vote_count: u32,
    /// Total weight of votes for refund split.
    pub refund_split_weight: u64,
    /// Sum of refund split percentages (for calculating average).
    pub refund_split_sum: u64,
    /// Number of votes for refund split.
    pub refund_split_count: u32,
    /// Number of votes for malicious filing.
    pub malicious_weight: u64,
    /// Number of votes for malicious filing.
    pub malicious_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Dispute {
    pub id: u64,
    pub job_id: u64,
    pub client: Address,
    pub freelancer: Address,
    pub initiator: Address,
    pub reason: String,
    pub status: DisputeStatus,
    pub votes_for_client: u32,
    pub votes_for_freelancer: u32,
    pub votes_for_refund_split: u32,
    pub refund_split_sum: u64,
    /// Votes that the filing was malicious (bad-faith). Requires 4/5 supermajority to trigger.
    pub votes_for_malicious: u32,
    /// Votes for a proportional split award (SplitAward variant).
    pub votes_for_split_award: u32,
    pub min_votes: u32,
    pub tie_break_method: TieBreakMethod,
    pub created_at: u64,
    pub voting_deadline: u64,
    pub excluded_voters: Vec<Address>,
    /// List of arbitrators assigned to this dispute (randomly selected at creation)
    pub assigned_arbitrators: Vec<Address>,
    /// Incremental tally accumulator for O(1) verdict finalization.
    /// This is the authoritative source for vote weights during resolution.
    pub tally: DisputeTally,
    /// Number of arbitrators assigned to this dispute (max 7).
    pub arbitrator_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataKey {
    Dispute(u64),
    DisputeCount,
    Votes(u64),
    Voters(u64),
    LastDisputeClosedAt(u64),
    HasVoted(u64, Address),
    ReputationContract,
    MinVoterReputation,
    Admin,
    EscrowContract,
    Paused,
    SlashAmount,
    ReputationSlashBps,
    JobDispute(u64),
    JobDisputes(u64),
    /// Maps (owner, job_id) → delegate address. Lets the owner look up who they delegated to.
    VoteDelegation(Address, u64),
    /// Maps (delegate, job_id) → owner address. Used in cast_vote to resolve the owner.
    DelegationOwner(Address, u64),
    /// Records the ledger sequence when a dispute last closed for a (client, freelancer) pair.
    LastDisputeLedger(Address, Address),
    /// Admin-configurable cooldown duration in ledgers between disputes for the same party pair.
    CooldownDuration,
    /// Stores the DisputeTally for O(1) verdict finalization.
    DisputeTally(u64),
    /// Stores assigned arbitrators for a dispute: dispute_id → Vec<Address>
    Arbitrators(u64),
    /// Pool of eligible arbitrators that can be randomly selected for disputes
    ArbitratorPool,
    /// Maps dispute_id → appeal_id (one appeal per dispute).
    DisputeAppeal(u64),
    /// Global monotonic appeal counter.
    AppealCount,
    /// Maps appeal_id → Appeal struct.
    Appeal(u64),
    /// Maps appeal_id → Vec<Vote>.
    AppealVotes(u64),
    /// Tracks whether a voter has already voted on a given appeal.
    HasVotedAppeal(u64, Address),
    /// Per-caller nonce to prevent replay attacks within the TTL window.
    Nonce(Address, Symbol, u64),
    /// Stores the resolved split ratio for audit when a tie produces a 50/50 split.
    SplitRatio(u64),
    /// Maps dispute_id → Vec<EvidenceRecord> for all submitted evidence.
    Evidence(u64),
    /// Caches the intended `DisputeResolution` when the escrow callback fails; cleared on retry success.
    PendingResolution(u64),
    ExclusionProposal(u64, Address),
}

fn require_not_paused(env: &Env) -> Result<(), DisputeError> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(DisputeError::ContractPaused);
    }
    Ok(())
}

fn require_admin(env: &Env, admin: &Address) -> Result<(), DisputeError> {
    let stored_admin: Address = env
        .storage()
        .instance()
        .get(&DataKey::Admin)
        .ok_or(DisputeError::NotInitialized)?;

    if admin != &stored_admin {
        return Err(DisputeError::NotAdmin);
    }
    Ok(())
}

const MIN_VOTER_REPUTATION: u32 = 300;

/// Default reputation points slashed from the losing party after a resolved dispute.
const DEFAULT_SLASH_AMOUNT: u64 = 50;
const DEFAULT_REPUTATION_SLASH_BPS: u32 = 500; // 5%
const DISPUTE_COOLDOWN_SECS: u64 = 86_400;
const VOTING_PERIOD_SECS: u64 = 604_800; // 7 days
const DEFAULT_PARTY_COOLDOWN_SECS: u64 = 1_209_600; // 14 days

/// Maximum number of jobs to look back for conflict detection to avoid instruction limits.
const MAX_CONFLICT_LOOKBACK: u64 = 100;

/// How long (in seconds) after dispute resolution an appeal may be filed.
const APPEAL_WINDOW_SECS: u64 = 172_800; // 48 hours
/// Minimum votes required to resolve an appeal.
const APPEAL_MIN_VOTES: u32 = 3;

/// Minimum total votes before a MaliciousFiling determination can trigger
/// (issue #1169). Below this, the ratio check alone could fire on a single
/// stray vote (e.g. 1/1 = 100%).
const MALICIOUS_FILING_MIN_VOTES: u32 = 5;
/// Numerator of the malicious-filing supermajority ratio: requires
/// `votes_for_malicious * MALICIOUS_FILING_SUPERMAJORITY_NUM >=
/// total_votes * MALICIOUS_FILING_SUPERMAJORITY_DENOM`, i.e. ≥ 4/5 (80%).
const MALICIOUS_FILING_SUPERMAJORITY_NUM: u32 = 4;
/// Denominator of the malicious-filing supermajority ratio (see above).
const MALICIOUS_FILING_SUPERMAJORITY_DENOM: u32 = 5;

const NONCE_EXPIRY_LEDGERS: u32 = 3;

/// Production TTL sizing for dispute-related storage, based on Stellar's
/// ~5-second ledger close time and mirroring the escrow contract's
/// `LEDGERS_PER_DAY` pattern.
///
/// A dispute can remain live for roughly **16 days** in the worst case: a
/// 7-day voting period, a 48-hour appeal window after resolution, and a
/// further 7-day appeal voting period. The per-dispute persistent storage TTL
/// must comfortably exceed that window so a long-idle dispute — no interaction
/// for hours, which is normal early in the voting window — is never archived by
/// the ledger before it is resolved or appealed. (The previous 1,000/10,000
/// ledger constants covered only ~14 hours, far short of the lifecycle.)
const LEDGERS_PER_DAY: u32 = 17_280; // 86,400 seconds/day ÷ 5 seconds/ledger
const MIN_TTL_THRESHOLD: u32 = LEDGERS_PER_DAY * 21; // 21 days = 362,880 ledgers
const MIN_TTL_EXTEND_TO: u32 = LEDGERS_PER_DAY * 30; // 30 days = 518,400 ledgers

/// Instance storage holds global monotonic counters (DisputeCount, AppealCount,
/// arbitrator pool, pause flag). Give it a very long TTL so those counters
/// survive extended contract idleness, matching the escrow contract's instance
/// storage sizing.
const INSTANCE_TTL_THRESHOLD: u32 = 50_000_000;
const INSTANCE_TTL_EXTEND_TO: u32 = 50_000_000;

fn consume_nonce(env: &Env, caller: &Address, function: &Symbol, nonce: u64) -> Result<(), DisputeError> {
    let key = DataKey::Nonce(caller.clone(), function.clone(), nonce);
    if env.storage().temporary().has(&key) {
        return Err(DisputeError::NonceReplay);
    }
    env.storage().temporary().set(&key, &true);
    env.storage().temporary().extend_ttl(&key, NONCE_EXPIRY_LEDGERS, NONCE_EXPIRY_LEDGERS);
    Ok(())
}

fn bump_dispute_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::Dispute(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_votes_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::Votes(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_last_dispute_closed_ttl(env: &Env, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::LastDisputeClosedAt(job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_has_voted_ttl(env: &Env, dispute_id: u64, voter: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::HasVoted(dispute_id, voter.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_dispute_count_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL_THRESHOLD, INSTANCE_TTL_EXTEND_TO);
}

fn bump_job_dispute_ttl(env: &Env, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::JobDispute(job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_job_disputes_ttl(env: &Env, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::JobDisputes(job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_vote_delegation_ttl(env: &Env, owner: &Address, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::VoteDelegation(owner.clone(), job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_delegation_owner_ttl(env: &Env, delegate: &Address, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::DelegationOwner(delegate.clone(), job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_last_dispute_ledger_ttl(env: &Env, client: &Address, freelancer: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::LastDisputeLedger(client.clone(), freelancer.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_dispute_tally_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::DisputeTally(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_arbitrators_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::Arbitrators(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_evidence_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::Evidence(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_pending_resolution_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::PendingResolution(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

/// Every other `bump_*_ttl` helper extends only when the entry's remaining
/// TTL has fallen below `MIN_TTL_THRESHOLD`, because that key gets touched
/// repeatedly over a dispute's lifetime and each touch is another chance to
/// refresh it. `ExclusionProposal` has no such second chance: it exists for
/// exactly one write (this call) and is either confirmed and removed, or
/// never touched again. A fresh persistent entry's ledger-assigned baseline
/// TTL already exceeds `MIN_TTL_THRESHOLD`, so extending with that same
/// threshold here would be a guaranteed no-op on the very write meant to
/// protect it — passing `MIN_TTL_EXTEND_TO` as the threshold instead forces
/// the extension to actually take effect immediately (#1166).
fn bump_exclusion_proposal_ttl(env: &Env, dispute_id: u64, voter: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::ExclusionProposal(dispute_id, voter.clone()),
        MIN_TTL_EXTEND_TO,
        MIN_TTL_EXTEND_TO,
    );
}

/// Creates a default (zeroed) DisputeTally for a new dispute.
fn new_tally() -> DisputeTally {
    DisputeTally {
        client_weight: 0,
        freelancer_weight: 0,
        total_weight_cast: 0,
        vote_count: 0,
        refund_split_weight: 0,
        refund_split_sum: 0,
        refund_split_count: 0,
        malicious_weight: 0,
        malicious_count: 0,
    }
}

fn bump_appeal_ttl(env: &Env, appeal_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::Appeal(appeal_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_appeal_votes_ttl(env: &Env, appeal_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::AppealVotes(appeal_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_dispute_appeal_ttl(env: &Env, dispute_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::DisputeAppeal(dispute_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_has_voted_appeal_ttl(env: &Env, appeal_id: u64, voter: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::HasVotedAppeal(appeal_id, voter.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

// Import Job struct from escrow for cross-contract calls
mod escrow {
    use soroban_sdk::{contracttype, Address, String, Vec};

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
    pub enum MilestoneStatus {
        Pending,
        InProgress,
        Submitted,
        Approved,
        PartiallyPaid,
    }

    #[contracttype]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Milestone {
        pub id: u32,
        pub description: String,
        pub amount: i128,
        pub status: MilestoneStatus,
        pub deadline: u64,
    }

    #[contracttype]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Job {
        pub id: u64,
        pub client: Address,
        pub freelancer: Address,
        pub token: Address,
        pub total_amount: i128,
        pub funded_amount: i128,
        pub status: JobStatus,
        pub milestones: Vec<Milestone>,
        pub job_deadline: u64,
        pub auto_refund_after: u64,
    }
}

fn detect_conflicts(
    env: &Env,
    escrow_contract: &Address,
    client: &Address,
    freelancer: &Address,
) -> Vec<Address> {
    let mut conflicts = Vec::<Address>::new(env);

    // Query job count from escrow
    let job_count_result = env.try_invoke_contract::<u64, soroban_sdk::Error>(
        escrow_contract,
        &Symbol::new(env, "get_job_count"),
        vec![env],
    );

    let job_count = match job_count_result {
        Ok(Ok(count)) => count,
        _ => return conflicts, // Return empty if query fails
    };

    if job_count == 0 {
        return conflicts;
    }

    // Determine starting job_id for lookback
    let start_id = if job_count > MAX_CONFLICT_LOOKBACK {
        job_count - MAX_CONFLICT_LOOKBACK + 1
    } else {
        1
    };

    // Iterate through recent jobs
    for job_id in start_id..=job_count {
        let job_result = env.try_invoke_contract::<escrow::Job, soroban_sdk::Error>(
            escrow_contract,
            &Symbol::new(env, "get_job"),
            vec![env, job_id.into_val(env)],
        );

        let job = match job_result {
            Ok(Ok(j)) => j,
            _ => continue, // Skip failed queries
        };

        // Check if job involves dispute client
        if &job.client == client || &job.freelancer == client {
            // Add the other party if not already in conflicts and not the dispute freelancer
            if &job.client != client
                && &job.client != freelancer
                && !conflicts.contains(&job.client)
            {
                conflicts.push_back(job.client.clone());
            }
            if &job.freelancer != client
                && &job.freelancer != freelancer
                && !conflicts.contains(&job.freelancer)
            {
                conflicts.push_back(job.freelancer.clone());
            }
        }

        // Check if job involves dispute freelancer
        if &job.client == freelancer || &job.freelancer == freelancer {
            // Add the other party if not already in conflicts and not the dispute client
            if &job.client != freelancer
                && &job.client != client
                && !conflicts.contains(&job.client)
            {
                conflicts.push_back(job.client.clone());
            }
            if &job.freelancer != freelancer
                && &job.freelancer != client
                && !conflicts.contains(&job.freelancer)
            {
                conflicts.push_back(job.freelancer.clone());
            }
        }
    }

    conflicts
}

/// Select random arbitrators from the pool, excluding conflicted parties.
/// Uses a simple pseudo-random selection based on ledger timestamp and dispute ID.
fn select_arbitrators(
    env: &Env,
    dispute_id: u64,
    excluded: &Vec<Address>,
    client: &Address,
    freelancer: &Address,
    count: u32,
) -> Vec<Address> {
    let pool: Vec<Address> = env
        .storage()
        .instance()
        .get(&DataKey::ArbitratorPool)
        .unwrap_or(Vec::new(env));

    let mut selected = Vec::<Address>::new(env);
    
    if pool.is_empty() {
        return selected;
    }

    // Filter out excluded addresses, client, and freelancer
    let mut eligible = Vec::<Address>::new(env);
    for addr in pool.iter() {
        if !excluded.contains(&addr) 
            && &addr != client 
            && &addr != freelancer 
        {
            eligible.push_back(addr);
        }
    }

    if eligible.is_empty() {
        return selected;
    }

    // Use ledger timestamp and dispute_id as pseudo-random seed
    let seed = env.ledger().timestamp().wrapping_add(dispute_id);
    let pool_size = eligible.len() as u64;
    
    // Select up to 'count' unique arbitrators
    let mut attempts = 0u32;
    let max_attempts = count.saturating_mul(3); // Prevent infinite loops
    
    while selected.len() < count as u32 && attempts < max_attempts && selected.len() < eligible.len() as u32 {
        let index = ((seed.wrapping_add(attempts as u64).wrapping_mul(2654435761)) % pool_size) as u32;
        let candidate = eligible.get(index).unwrap();
        
        if !selected.contains(&candidate) {
            selected.push_back(candidate);
        }
        attempts += 1;
    }

    selected
}

fn select_replacement_arbitrator(env: &Env, dispute: &Dispute) -> Option<Address> {
    let pool: Vec<Address> = env
        .storage()
        .instance()
        .get(&DataKey::ArbitratorPool)
        .unwrap_or(Vec::new(env));

    for candidate in pool.iter() {
        if candidate != dispute.client
            && candidate != dispute.freelancer
            && !dispute.assigned_arbitrators.contains(&candidate)
            && !dispute.excluded_voters.contains(&candidate)
        {
            return Some(candidate);
        }
    }
    None
}

#[contract]
pub struct DisputeContract;

#[contractimpl]
impl DisputeContract {
    /// Initialize the dispute contract with reputation contract address and admin.
    pub fn initialize(
        env: Env,
        admin: Address,
        reputation_contract: Address,
        min_voter_reputation: u32,
        escrow_contract: Address,
    ) -> Result<(), DisputeError> {
        admin.require_auth();

        // Check if already initialized
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(DisputeError::Unauthorized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::ReputationContract, &reputation_contract);
        env.storage()
            .instance()
            .set(&DataKey::MinVoterReputation, &min_voter_reputation);
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &escrow_contract);
        env.storage().instance().set(&DataKey::Paused, &false);
        env.storage()
            .instance()
            .set(&DataKey::SlashAmount, &DEFAULT_SLASH_AMOUNT);
        env.storage()
            .instance()
            .set(&DataKey::ReputationSlashBps, &DEFAULT_REPUTATION_SLASH_BPS);

        bump_dispute_count_ttl(&env);

        // Emit event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("init")),
            (admin, reputation_contract, min_voter_reputation),
        );

        Ok(())
    }

    /// Pause the contract (admin only).
    pub fn pause(env: Env, admin: Address) -> Result<(), DisputeError> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        env.storage().instance().set(&DataKey::Paused, &true);
        bump_dispute_count_ttl(&env);

        // Emit event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("paused")),
            (admin, env.ledger().timestamp()),
        );

        Ok(())
    }

    /// Unpause the contract (admin only).
    pub fn unpause(env: Env, admin: Address) -> Result<(), DisputeError> {
        admin.require_auth();
        require_admin(&env, &admin)?;

        env.storage().instance().set(&DataKey::Paused, &false);
        bump_dispute_count_ttl(&env);

        // Emit event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("unpaused")),
            (admin, env.ledger().timestamp()),
        );

        Ok(())
    }

    /// Set the minimum voter reputation threshold (admin only).
    pub fn set_min_voter_reputation(
        env: Env,
        admin: Address,
        min_reputation: u32,
    ) -> Result<(), DisputeError> {
        admin.require_auth();
        require_not_paused(&env)?;
        require_admin(&env, &admin)?;

        env.storage()
            .instance()
            .set(&DataKey::MinVoterReputation, &min_reputation);
        bump_dispute_count_ttl(&env);

        // Emit event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("minrep")),
            (admin, min_reputation),
        );

        Ok(())
    }

    /// Set the per-party-pair cooldown duration in seconds (admin only).
    pub fn set_cooldown_duration(env: Env, admin: Address, seconds: u64) -> Result<(), DisputeError> {
        admin.require_auth();
        require_not_paused(&env)?;
        require_admin(&env, &admin)?;

        env.storage().instance().set(&DataKey::CooldownDuration, &seconds);
        bump_dispute_count_ttl(&env);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("cooldown")),
            (admin, seconds),
        );

        Ok(())
    }

    /// Check if an address is eligible to vote based on reputation.
    pub fn is_eligible_voter(env: Env, voter: Address) -> Result<bool, DisputeError> {
        let reputation_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::ReputationContract)
            .ok_or(DisputeError::NotInitialized)?;

        let min_reputation: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MinVoterReputation)
            .unwrap_or(MIN_VOTER_REPUTATION);

        // Call reputation contract to get voter's reputation
        let reputation_result = env
            .try_invoke_contract::<reputation::UserReputation, soroban_sdk::Error>(
                &reputation_contract,
                &Symbol::new(&env, "get_reputation"),
                vec![&env, voter.into_val(&env)],
            );

        match reputation_result {
            Ok(Ok(rep)) => {
                // Use total_score as the reputation metric
                Ok(rep.total_score >= min_reputation as u64)
            }
            _ => Ok(false), // User not found or error = not eligible
        }
    }

    /// Raise a dispute on a job. Either the client or freelancer can initiate.
    /// Automatically selects 5 random arbitrators from the pool to vote on this dispute.
    #[allow(clippy::too_many_arguments)]
    pub fn raise_dispute(
        env: Env,
        job_id: u64,
        client: Address,
        freelancer: Address,
        initiator: Address,
        reason: String,
        min_votes: u32,
        _tie_break_method: Option<TieBreakMethod>,
    ) -> Result<u64, DisputeError> {
        initiator.require_auth();
        require_not_paused(&env)?;

        if initiator != client && initiator != freelancer {
            return Err(DisputeError::InvalidParty);
        }

        if let Some(last_closed_at) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::LastDisputeClosedAt(job_id))
        {
            let now = env.ledger().timestamp();
            if now <= last_closed_at.saturating_add(DISPUTE_COOLDOWN_SECS) {
                return Err(DisputeError::DisputeCooldown);
            }
            bump_last_dispute_closed_ttl(&env, job_id);
        }

        // Per-party-pair cooldown: same client/freelancer pair cannot re-dispute until the
        // configured window has elapsed since the last dispute resolved between them.
        let party_cooldown: u64 = env
            .storage()
            .instance()
            .get(&DataKey::CooldownDuration)
            .unwrap_or(DEFAULT_PARTY_COOLDOWN_SECS);
        if let Some(last_ts) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::LastDisputeLedger(client.clone(), freelancer.clone()))
        {
            if env.ledger().timestamp() < last_ts.saturating_add(party_cooldown) {
                return Err(DisputeError::DisputeCooldown);
            }
            bump_last_dispute_ledger_ttl(&env, &client, &freelancer);
        }

        let mut count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::DisputeCount)
            .unwrap_or(0);
        count += 1;

        // Detect conflicts of interest by querying escrow contract
        let excluded_voters = if let Some(escrow_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::EscrowContract)
        {
            detect_conflicts(&env, &escrow_contract, &client, &freelancer)
        } else {
            Vec::<Address>::new(&env)
        };

        // Select random arbitrators for this dispute
        let assigned_arbitrators = select_arbitrators(&env, count, &excluded_voters, &client, &freelancer, ARBITRATORS_PER_DISPUTE);

        let dispute = Dispute {
            id: count,
            job_id,
            client: client.clone(),
            freelancer: freelancer.clone(),
            initiator: initiator.clone(),
            reason,
            status: DisputeStatus::Open,
            votes_for_client: 0,
            votes_for_freelancer: 0,
            votes_for_refund_split: 0,
            refund_split_sum: 0,
            votes_for_malicious: 0,
            votes_for_split_award: 0,
            min_votes: if min_votes < AUTO_RESOLVE_VOTE_THRESHOLD { AUTO_RESOLVE_VOTE_THRESHOLD } else { min_votes },
            tie_break_method: TieBreakMethod::RefundBoth,
            created_at: env.ledger().timestamp(),
            voting_deadline: env.ledger().timestamp().saturating_add(VOTING_PERIOD_SECS),
            excluded_voters,
            assigned_arbitrators: assigned_arbitrators.clone(),
            tally: new_tally(),
            arbitrator_count: assigned_arbitrators.len() as u32,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(count), &dispute);
        env.storage().instance().set(&DataKey::DisputeCount, &count);
        bump_dispute_ttl(&env, count);
        bump_dispute_count_ttl(&env);
        env.storage()
            .persistent()
            .set(&DataKey::Votes(count), &Vec::<Vote>::new(&env));
        bump_votes_ttl(&env, count);

        // Initialize DisputeTally for O(1) verdict finalization
        env.storage()
            .persistent()
            .set(&DataKey::DisputeTally(count), &new_tally());
        bump_dispute_tally_ttl(&env, count);

        // Store assigned arbitrators for this dispute
        env.storage()
            .persistent()
            .set(&DataKey::Arbitrators(count), &assigned_arbitrators);
        bump_arbitrators_ttl(&env, count);

        // Maintain job → dispute_id mapping so callers can look up a dispute by job_id
        env.storage()
            .persistent()
            .set(&DataKey::JobDispute(job_id), &count);
        bump_job_dispute_ttl(&env, job_id);

        // Maintain job → dispute_ids list so callers can fetch historical disputes.
        let list_key = DataKey::JobDisputes(job_id);
        let mut dispute_ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&list_key)
            .unwrap_or(Vec::new(&env));
        dispute_ids.push_back(count);
        env.storage().persistent().set(&list_key, &dispute_ids);
        bump_job_disputes_ttl(&env, job_id);

        // Emit event with assigned arbitrators
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("raised")),
            (count, job_id, initiator, client, freelancer, assigned_arbitrators),
        );

        // Notify the escrow contract so it can transition the job to Disputed and emit
        // a structured escrow-side DisputeRaised event that indexers can consume.
        if let Some(escrow_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::EscrowContract)
        {
            let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
                &escrow_contract,
                &Symbol::new(&env, "mark_job_disputed"),
                vec![&env, job_id.into_val(&env), count.into_val(&env)],
            );
        }

        Ok(count)
    }

    /// Cast a vote on a dispute. Only assigned arbitrators can vote.
    /// If the reputation system is initialized, voters must meet the minimum
    /// reputation threshold. When no reputation contract is configured, voting
    /// proceeds without a reputation check to allow graceful degradation.
    /// Auto-resolves the dispute when 3 votes are cast for the same decision.
    pub fn cast_vote(
        env: Env,
        dispute_id: u64,
        voter: Address,
        choice: VoteChoice,
        reason: String,
        nonce: u64,
    ) -> Result<(), DisputeError> {
        consume_nonce(&env, &voter, &Symbol::new(&env, "cast_vote"), nonce)?;
        voter.require_auth();
        require_not_paused(&env)?;

        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        if dispute.status != DisputeStatus::Open && dispute.status != DisputeStatus::Voting {
            return Err(DisputeError::VotingClosed);
        }

        // Resolve delegation first: if the voter is acting as a delegate, look up the
        // stake owner so the arbitrator-membership and double-vote checks use the owner.
        let delegation_owner: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::DelegationOwner(voter.clone(), dispute.job_id));

        let stake_owner = delegation_owner.as_ref().unwrap_or(&voter);

        // Check if the effective arbitrator (the owner when delegated, otherwise the voter)
        // is an assigned arbitrator for this dispute.
        if !dispute.assigned_arbitrators.contains(stake_owner) {
            return Err(DisputeError::Unauthorized);
        }

        // Parties involved cannot vote
        if stake_owner == &dispute.client || stake_owner == &dispute.freelancer {
            return Err(DisputeError::ConflictOfInterest);
        }

        // Check if voter or their principal is excluded due to conflict of interest
        if dispute.excluded_voters.contains(&voter)
            || (delegation_owner.is_some() && dispute.excluded_voters.contains(stake_owner))
        {
            return Err(DisputeError::ConflictOfInterest);
        }

        // Check voter reputation eligibility against the stake owner (owner if delegated).
        if env.storage().instance().has(&DataKey::ReputationContract) {
            let is_eligible = Self::is_eligible_voter(env.clone(), stake_owner.clone())?;
            if !is_eligible {
                return Err(DisputeError::InsufficientReputation);
            }
        }

        // Prevent double-vote: check both the physical voter and the stake owner.
        let voted_key = DataKey::HasVoted(dispute_id, voter.clone());
        if env.storage().persistent().has(&voted_key) {
            return Err(DisputeError::AlreadyVoted);
        }
        if delegation_owner.is_some() {
            let owner_voted_key = DataKey::HasVoted(dispute_id, stake_owner.clone());
            if env.storage().persistent().has(&owner_voted_key) {
                return Err(DisputeError::AlreadyVoted);
            }
        }

        // Maintain Voters set
        let mut voters: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Voters(dispute_id))
            .unwrap_or(Vec::new(&env));
        if voters.contains(&voter) {
            return Err(DisputeError::AlreadyVoted);
        }
        voters.push_back(voter.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Voters(dispute_id), &voters);

        // Record vote
        let vote = Vote {
            voter: voter.clone(),
            choice: choice.clone(),
            reason,
            timestamp: env.ledger().timestamp(),
        };

        let mut votes: Vec<Vote> = env
            .storage()
            .persistent()
            .get(&DataKey::Votes(dispute_id))
            .unwrap_or(Vec::new(&env));
        votes.push_back(vote);
        env.storage()
            .persistent()
            .set(&DataKey::Votes(dispute_id), &votes);
        bump_votes_ttl(&env, dispute_id);

        match choice {
            VoteChoice::Client => dispute.votes_for_client += 1,
            VoteChoice::Freelancer => dispute.votes_for_freelancer += 1,
            VoteChoice::RefundSplit(pct_client) => {
                if pct_client > 100 {
                    return Err(DisputeError::InvalidSplitBps);
                }
                dispute.votes_for_refund_split += 1;
                dispute.refund_split_sum =
                    dispute.refund_split_sum.saturating_add(pct_client as u64);
            }
            VoteChoice::MaliciousFiling => dispute.votes_for_malicious += 1,
            VoteChoice::SplitAward(client_bps, freelancer_bps) => {
                if client_bps.saturating_add(freelancer_bps) != 10_000 {
                    return Err(DisputeError::InvalidSplitBps);
                }
                dispute.votes_for_split_award += 1;
            }
        }

        // Update DisputeTally with O(1) incremental accumulator
        let mut tally: DisputeTally = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeTally(dispute_id))
            .unwrap_or_else(|| new_tally());
        bump_dispute_tally_ttl(&env, dispute_id);

        // For now, weight = 1 for each vote (uniform weighting).
        // Future enhancement: weight can be reputation-based or stake-based.
        let vote_weight: u64 = 1;

        match choice {
            VoteChoice::Client => {
                tally.client_weight = tally.client_weight.saturating_add(vote_weight);
            }
            VoteChoice::Freelancer => {
                tally.freelancer_weight = tally.freelancer_weight.saturating_add(vote_weight);
            }
            VoteChoice::RefundSplit(pct_client) => {
                tally.refund_split_weight = tally.refund_split_weight.saturating_add(vote_weight);
                tally.refund_split_sum = tally.refund_split_sum.saturating_add(pct_client as u64);
                tally.refund_split_count += 1;
            }
            VoteChoice::MaliciousFiling => {
                tally.malicious_weight = tally.malicious_weight.saturating_add(vote_weight);
                tally.malicious_count += 1;
            }
            VoteChoice::SplitAward(_client_bps, _freelancer_bps) => {
                // SplitAward votes are counted separately in votes_for_split_award
                // The tally tracks them as a distinct vote category
                tally.refund_split_weight = tally.refund_split_weight.saturating_add(vote_weight);
            }
        }

        tally.total_weight_cast = tally.total_weight_cast.saturating_add(vote_weight);
        tally.vote_count += 1;

        // Store updated tally
        env.storage()
            .persistent()
            .set(&DataKey::DisputeTally(dispute_id), &tally);
        bump_dispute_tally_ttl(&env, dispute_id);

        // Update dispute tally field for consistency
        dispute.tally = tally;

        dispute.status = DisputeStatus::Voting;
        env.storage()
            .persistent()
            .set(&DataKey::Dispute(dispute_id), &dispute);
        env.storage().persistent().set(&voted_key, &true);
        bump_dispute_ttl(&env, dispute_id);
        bump_has_voted_ttl(&env, dispute_id, &voter);

        // When voting via delegation, also mark the owner as having voted so they
        // cannot vote again directly and the delegation cannot be reused.
        if let Some(ref owner) = delegation_owner {
            let owner_voted_key = DataKey::HasVoted(dispute_id, owner.clone());
            env.storage().persistent().set(&owner_voted_key, &true);
            bump_has_voted_ttl(&env, dispute_id, owner);
        }

        // Emit VoteCast event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("voted")),
            (dispute_id, voter.clone(), choice.clone(), dispute.job_id, dispute.client.clone(), dispute.freelancer.clone()),
        );

        // Auto-resolve if threshold votes reached for the same decision (majority threshold)
        let escrow_addr: Option<Address> = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract);

        if let Some(escrow) = escrow_addr {
            if dispute.votes_for_client >= AUTO_RESOLVE_VOTE_THRESHOLD
                || dispute.votes_for_freelancer >= AUTO_RESOLVE_VOTE_THRESHOLD
                || dispute.votes_for_refund_split >= AUTO_RESOLVE_VOTE_THRESHOLD
                || dispute.votes_for_malicious >= AUTO_RESOLVE_VOTE_THRESHOLD
                || dispute.votes_for_split_award >= AUTO_RESOLVE_VOTE_THRESHOLD
            {
                // Auto-resolve the dispute
                let _ = internal_resolve(&env, dispute_id, &mut dispute, &escrow, false);
            }
        }

        Ok(())
    }

    /// Propose or confirm an assigned arbitrator's exclusion while voting is open.
    /// Both parties must agree and an eligible replacement must be available.
    pub fn add_excluded_voter(
        env: Env,
        dispute_id: u64,
        caller: Address,
        voter: Address,
    ) -> Result<(), DisputeError> {
        caller.require_auth();
        require_not_paused(&env)?;

        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        // Verify caller is either client or freelancer
        if caller != dispute.client && caller != dispute.freelancer {
            return Err(DisputeError::Unauthorized);
        }

        // Check dispute status is Open
        if dispute.status != DisputeStatus::Open {
            return Err(DisputeError::VotingClosed);
        }

        if !dispute.assigned_arbitrators.contains(&voter) {
            return Err(DisputeError::InvalidArbitrator);
        }

        let proposal_key = DataKey::ExclusionProposal(dispute_id, voter.clone());
        let proposer: Option<Address> = env.storage().persistent().get(&proposal_key);
        if proposer.is_none() {
            env.storage().persistent().set(&proposal_key, &caller);
            // Every other persistent key in this contract gets its TTL
            // extended on write — an exclusion proposal awaiting the second
            // party's confirmation is no different, and without this it
            // could expire from storage before that confirmation ever
            // happens (#1166).
            bump_exclusion_proposal_ttl(&env, dispute_id, &voter);
            return Ok(());
        }
        if proposer == Some(caller) {
            return Err(DisputeError::ExclusionNotConfirmed);
        }

        let replacement = select_replacement_arbitrator(&env, &dispute)
            .ok_or(DisputeError::ReplacementUnavailable)?;
        let mut updated_arbitrators = Vec::<Address>::new(&env);
        for assigned in dispute.assigned_arbitrators.iter() {
            if assigned == voter {
                updated_arbitrators.push_back(replacement.clone());
            } else {
                updated_arbitrators.push_back(assigned);
            }
        }
        dispute.excluded_voters.push_back(voter.clone());
        dispute.assigned_arbitrators = updated_arbitrators.clone();
        dispute.arbitrator_count = updated_arbitrators.len();

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(dispute_id), &dispute);
        env.storage()
            .persistent()
            .set(&DataKey::Arbitrators(dispute_id), &updated_arbitrators);
        env.storage().persistent().remove(&proposal_key);
        bump_dispute_ttl(&env, dispute_id);
        bump_arbitrators_ttl(&env, dispute_id);

        // Emit event
        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("excluded")),
            (
                dispute_id,
                voter,
                dispute.job_id,
                dispute.client,
                dispute.freelancer,
                replacement,
            ),
        );

        Ok(())
    }

    pub fn resolve_dispute(env: Env, dispute_id: u64) -> Result<DisputeStatus, DisputeError> {
        require_not_paused(&env)?;

        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        let escrow_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .ok_or(DisputeError::NotInitialized)?;

        internal_resolve(&env, dispute_id, &mut dispute, &escrow_addr, false)
    }

    /// Force resolution of a dispute after the voting deadline has passed,
    /// even if the minimum number of votes has not been reached.
    pub fn force_resolve_timeout(env: Env, dispute_id: u64) -> Result<DisputeStatus, DisputeError> {
        require_not_paused(&env)?;

        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        // Retrieve the trusted escrow contract address from storage
        let escrow_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .ok_or(DisputeError::NotInitialized)?;

        if env.ledger().timestamp() < dispute.voting_deadline {
            return Err(DisputeError::VotingPeriodNotExpired);
        }

        let mut active_arbitrators = 0u32;
        for arbitrator in dispute.assigned_arbitrators.iter() {
            if !dispute.excluded_voters.contains(&arbitrator) {
                active_arbitrators = active_arbitrators.saturating_add(1);
            }
        }
        if active_arbitrators < AUTO_RESOLVE_VOTE_THRESHOLD {
            return Err(DisputeError::InsufficientActiveArbitrators);
        }

        internal_resolve(&env, dispute_id, &mut dispute, &escrow_addr, true)
    }

    /// File an appeal on a resolved dispute within the 48-hour appeal window.
    /// Only the client or freelancer may appeal. Each dispute can be appealed at most once.
    /// The original arbitrators are excluded from the appeal panel.
    pub fn appeal(
        env: Env,
        dispute_id: u64,
        appellant: Address,
    ) -> Result<u64, DisputeError> {
        appellant.require_auth();
        require_not_paused(&env)?;

        let dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        if appellant != dispute.client && appellant != dispute.freelancer {
            return Err(DisputeError::InvalidParty);
        }

        // Dispute must be resolved before it can be appealed.
        let is_resolved = matches!(
            dispute.status,
            DisputeStatus::ResolvedForClient
                | DisputeStatus::ResolvedForFreelancer
                | DisputeStatus::RefundedBoth
                | DisputeStatus::Escalated
        ) || matches!(dispute.status, DisputeStatus::RefundSplit(_));
        if !is_resolved {
            return Err(DisputeError::VotingClosed);
        }

        // Enforce 48-hour appeal window from when the dispute was closed.
        let resolved_at: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::LastDisputeClosedAt(dispute.job_id))
            .unwrap_or(0);
        if env.ledger().timestamp() > resolved_at.saturating_add(APPEAL_WINDOW_SECS) {
            return Err(DisputeError::AppealWindowExpired);
        }

        // Each dispute may only be appealed once.
        if env.storage().persistent().has(&DataKey::DisputeAppeal(dispute_id)) {
            return Err(DisputeError::AlreadyAppealed);
        }

        // Snapshot the original arbitrators to exclude them from the appeal panel.
        let excluded = Self::get_arbitrators(env.clone(), dispute_id);

        let mut appeal_count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::AppealCount)
            .unwrap_or(0);
        appeal_count += 1;

        let now = env.ledger().timestamp();
        let new_appeal = Appeal {
            id: appeal_count,
            dispute_id,
            appellant: appellant.clone(),
            excluded_arbitrators: excluded,
            status: AppealStatus::Open,
            votes_for_client: 0,
            votes_for_freelancer: 0,
            votes_for_refund_split: 0,
            refund_split_sum: 0,
            created_at: now,
            voting_deadline: now.saturating_add(VOTING_PERIOD_SECS),
        };

        env.storage().persistent().set(&DataKey::Appeal(appeal_count), &new_appeal);
        env.storage().instance().set(&DataKey::AppealCount, &appeal_count);
        env.storage().persistent().set(&DataKey::DisputeAppeal(dispute_id), &appeal_count);
        env.storage().persistent().set(&DataKey::AppealVotes(appeal_count), &Vec::<Vote>::new(&env));
        bump_appeal_ttl(&env, appeal_count);
        bump_appeal_votes_ttl(&env, appeal_count);
        bump_dispute_appeal_ttl(&env, dispute_id);
        bump_dispute_count_ttl(&env);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("appealed")),
            (appeal_count, dispute_id, appellant),
        );

        Ok(appeal_count)
    }

    /// Cast a vote on an appeal. Original arbitrators from the base dispute are excluded.
    pub fn cast_appeal_vote(
        env: Env,
        appeal_id: u64,
        voter: Address,
        choice: VoteChoice,
        reason: String,
    ) -> Result<(), DisputeError> {
        voter.require_auth();
        require_not_paused(&env)?;

        let mut ap: Appeal = env
            .storage()
            .persistent()
            .get(&DataKey::Appeal(appeal_id))
            .ok_or(DisputeError::AppealNotFound)?;
        bump_appeal_ttl(&env, appeal_id);

        if ap.status != AppealStatus::Open && ap.status != AppealStatus::Voting {
            return Err(DisputeError::VotingClosed);
        }

        // Load the original dispute to block parties from voting.
        let dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(ap.dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;

        if voter == dispute.client || voter == dispute.freelancer {
            return Err(DisputeError::InvalidParty);
        }

        // Block original arbitrators from the appeal panel.
        if ap.excluded_arbitrators.contains(&voter) {
            return Err(DisputeError::ConflictOfInterest);
        }

        let voted_key = DataKey::HasVotedAppeal(appeal_id, voter.clone());
        if env.storage().persistent().has(&voted_key) {
            return Err(DisputeError::AlreadyVoted);
        }

        let vote = Vote {
            voter: voter.clone(),
            choice: choice.clone(),
            reason,
            timestamp: env.ledger().timestamp(),
        };

        let mut votes: Vec<Vote> = env
            .storage()
            .persistent()
            .get(&DataKey::AppealVotes(appeal_id))
            .unwrap_or(Vec::new(&env));
        votes.push_back(vote);
        env.storage().persistent().set(&DataKey::AppealVotes(appeal_id), &votes);
        bump_appeal_votes_ttl(&env, appeal_id);

        match choice {
            VoteChoice::Client => ap.votes_for_client += 1,
            VoteChoice::Freelancer => ap.votes_for_freelancer += 1,
            VoteChoice::RefundSplit(pct) => {
                if pct > 100 {
                    return Err(DisputeError::InvalidSplitBps);
                }
                ap.votes_for_refund_split += 1;
                ap.refund_split_sum = ap.refund_split_sum.saturating_add(pct as u64);
            }
            VoteChoice::MaliciousFiling | VoteChoice::SplitAward(_, _) => {
                return Err(DisputeError::Unauthorized);
            }
        }

        ap.status = AppealStatus::Voting;
        env.storage().persistent().set(&DataKey::Appeal(appeal_id), &ap);
        env.storage().persistent().set(&voted_key, &true);
        bump_appeal_ttl(&env, appeal_id);
        bump_has_voted_appeal_ttl(&env, appeal_id, &voter);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("ap_voted")),
            (appeal_id, voter, ap.dispute_id),
        );

        Ok(())
    }

    /// Resolve an appeal once enough votes have been cast.
    /// The decision is binding — it overwrites the original dispute's resolution.
    /// The losing party's reputation is slashed at double the normal rate.
    pub fn resolve_appeal(env: Env, appeal_id: u64) -> Result<AppealStatus, DisputeError> {
        require_not_paused(&env)?;

        let mut ap: Appeal = env
            .storage()
            .persistent()
            .get(&DataKey::Appeal(appeal_id))
            .ok_or(DisputeError::AppealNotFound)?;
        bump_appeal_ttl(&env, appeal_id);

        let already_resolved = matches!(
            ap.status,
            AppealStatus::ResolvedForClient
                | AppealStatus::ResolvedForFreelancer
                | AppealStatus::RefundedBoth
                | AppealStatus::Escalated
        ) || matches!(ap.status, AppealStatus::RefundSplit(_));
        if already_resolved {
            return Err(DisputeError::AlreadyResolved);
        }

        let total_votes =
            ap.votes_for_client + ap.votes_for_freelancer + ap.votes_for_refund_split;
        if total_votes < APPEAL_MIN_VOTES {
            return Err(DisputeError::NotEnoughVotes);
        }

        // Overwrite the original dispute's resolution — the appeal is final.
        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(ap.dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;

        // Determine the appeal outcome by plurality.
        if ap.votes_for_client > ap.votes_for_freelancer
            && ap.votes_for_client > ap.votes_for_refund_split
        {
            ap.status = AppealStatus::ResolvedForClient;
        } else if ap.votes_for_freelancer > ap.votes_for_client
            && ap.votes_for_freelancer > ap.votes_for_refund_split
        {
            ap.status = AppealStatus::ResolvedForFreelancer;
        } else if ap.votes_for_refund_split > ap.votes_for_client
            && ap.votes_for_refund_split > ap.votes_for_freelancer
        {
            let avg = ap.refund_split_sum / ap.votes_for_refund_split as u64;
            ap.status = AppealStatus::RefundSplit(avg as u32);
        } else {
            match dispute.tie_break_method {
                TieBreakMethod::FavorClient => ap.status = AppealStatus::ResolvedForClient,
                TieBreakMethod::FavorFreelancer => ap.status = AppealStatus::ResolvedForFreelancer,
                TieBreakMethod::RefundBoth => ap.status = AppealStatus::RefundedBoth,
                TieBreakMethod::Escalate => ap.status = AppealStatus::Escalated,
            }
        }

        let resolution = match ap.status {
            AppealStatus::ResolvedForClient => DisputeResolution::ClientWins,
            AppealStatus::ResolvedForFreelancer => DisputeResolution::FreelancerWins,
            AppealStatus::RefundedBoth => DisputeResolution::RefundBoth,
            AppealStatus::RefundSplit(pct) => DisputeResolution::RefundSplit(pct),
            _ => DisputeResolution::Escalate,
        };


        let dispute_outcome = match ap.status {
            AppealStatus::ResolvedForClient => DisputeStatus::ResolvedForClient,
            AppealStatus::ResolvedForFreelancer => DisputeStatus::ResolvedForFreelancer,
            AppealStatus::RefundedBoth => DisputeStatus::RefundedBoth,
            AppealStatus::RefundSplit(pct) => DisputeStatus::RefundSplit(pct),
            _ => DisputeStatus::Escalated,
        };
        dispute.status = dispute_outcome;
        env.storage().persistent().set(&DataKey::Dispute(ap.dispute_id), &dispute);
        bump_dispute_ttl(&env, ap.dispute_id);

        // Notify escrow of the (possibly revised) resolution.
        if resolution != DisputeResolution::Escalate {
            let escrow_addr: Address = env
                .storage()
                .instance()
                .get(&DataKey::EscrowContract)
                .ok_or(DisputeError::NotInitialized)?;

            let escrow_ok = matches!(
                env.try_invoke_contract::<(), soroban_sdk::Error>(
                    &escrow_addr,
                    &Symbol::new(&env, "resolve_dispute_callback"),
                    vec![&env, dispute.job_id.into_val(&env), resolution.clone().into_val(&env)],
                ),
                Ok(Ok(_))
            );

            if !escrow_ok {
                dispute.status = DisputeStatus::ResolutionFailed;
                env.storage()
                    .persistent()
                    .set(&DataKey::PendingResolution(ap.dispute_id), &resolution);
                bump_pending_resolution_ttl(&env, ap.dispute_id);
                env.storage()
                    .persistent()
                    .set(&DataKey::Dispute(ap.dispute_id), &dispute);
                bump_dispute_ttl(&env, ap.dispute_id);
                env.events().publish(
                    (symbol_short!("dispute"), Symbol::new(&env, "escrow_fail")),
                    (ap.dispute_id, dispute.job_id),
                );
                // Appeal itself is finalized; skip reputation slash and fall through to persist appeal.
            }

            // Double-rate reputation slash to deter frivolous appeals (only when escrow succeeded).
            if escrow_ok {
                if let Some(reputation_contract) = env
                    .storage()
                    .instance()
                    .get::<DataKey, Address>(&DataKey::ReputationContract)
                {
                    let loser = match resolution {
                        DisputeResolution::ClientWins => dispute.freelancer.clone(),
                        DisputeResolution::FreelancerWins => dispute.client.clone(),
                        _ => ap.appellant.clone(),
                    };

                    let slash_bps: u32 = env
                        .storage()
                        .instance()
                        .get(&DataKey::ReputationSlashBps)
                        .unwrap_or(DEFAULT_REPUTATION_SLASH_BPS);

                    let current_score = env
                        .try_invoke_contract::<reputation::UserReputation, soroban_sdk::Error>(
                            &reputation_contract,
                            &Symbol::new(&env, "get_reputation"),
                            vec![&env, loser.clone().into_val(&env)],
                        )
                        .ok()
                        .and_then(|r| r.ok())
                        .map(|r| r.total_score)
                        .unwrap_or(0);

                    // Double the slash rate for appeals.
                    let double_bps = slash_bps.saturating_mul(2);
                    let mut slash_amount: u64 =
                        (current_score.saturating_mul(double_bps as u64)) / 10_000;
                    if slash_amount == 0 && current_score > 0 {
                        slash_amount = 1;
                    }

                    let reason = String::from_str(&env, "appeal_lost");
                    let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
                        &reputation_contract,
                        &Symbol::new(&env, "slash_reputation"),
                        vec![
                            &env,
                            loser.clone().into_val(&env),
                            dispute.job_id.into_val(&env),
                            slash_amount.into_val(&env),
                            reason.into_val(&env),
                        ],
                    );

                    env.events().publish(
                        (symbol_short!("dispute"), Symbol::new(&env, "ap_slashed")),
                        (ap.id, loser, slash_amount),
                    );
                }
            }
        }

        env.storage().persistent().set(&DataKey::Appeal(appeal_id), &ap);
        bump_appeal_ttl(&env, appeal_id);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("ap_done")),
            (appeal_id, ap.status.clone(), ap.dispute_id),
        );

        Ok(ap.status.clone())
    }

    /// Get appeal details.
    pub fn get_appeal(env: Env, appeal_id: u64) -> Result<Appeal, DisputeError> {
        let ap: Appeal = env
            .storage()
            .persistent()
            .get(&DataKey::Appeal(appeal_id))
            .ok_or(DisputeError::AppealNotFound)?;
        bump_appeal_ttl(&env, appeal_id);
        Ok(ap)
    }

    /// Get dispute details.
    pub fn get_dispute(env: Env, dispute_id: u64) -> Result<Dispute, DisputeError> {
        let dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);
        Ok(dispute)
    }

    /// Look up the dispute associated with a given job ID.
    /// Returns `None` when no dispute has ever been raised for the job.
    /// When multiple disputes existed (e.g. after a cooldown period), this
    /// returns the most recent one because `raise_dispute` always overwrites
    /// the `JobDispute` mapping with the latest dispute ID.
    pub fn get_dispute_by_job(env: Env, job_id: u64) -> Option<Dispute> {
        let dispute_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey::JobDispute(job_id))?;
        bump_job_dispute_ttl(&env, job_id);

        let dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))?;
        bump_dispute_ttl(&env, dispute_id);

        Some(dispute)
    }

    /// Look up all historical disputes associated with a job.
    /// Returns an empty vec when no disputes have ever been raised.
    pub fn get_disputes_for_job(env: Env, job_id: u64) -> Vec<Dispute> {
        let ids: Vec<u64> = env
            .storage()
            .persistent()
            .get(&DataKey::JobDisputes(job_id))
            .unwrap_or(Vec::new(&env));
        if !ids.is_empty() {
            bump_job_disputes_ttl(&env, job_id);
        }

        let mut disputes = Vec::<Dispute>::new(&env);
        for id in ids.iter() {
            if let Some(dispute) = env
                .storage()
                .persistent()
                .get::<DataKey, Dispute>(&DataKey::Dispute(id))
            {
                bump_dispute_ttl(&env, id);
                disputes.push_back(dispute);
            }
        }

        disputes
    }

    /// Get all votes for a dispute.
    pub fn get_votes(env: Env, dispute_id: u64) -> Vec<Vote> {
        env.storage()
            .persistent()
            .get(&DataKey::Votes(dispute_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Submit evidence for an active dispute.
    ///
    /// Only the client or freelancer involved in the dispute may submit evidence.
    /// The dispute must be in `Open` or `Voting` status.
    /// Emits `EvidenceSubmitted` so indexers can track evidence without polling storage.
    pub fn submit_evidence(
        env: Env,
        dispute_id: u64,
        submitted_by: Address,
        evidence_hash: BytesN<32>,
    ) -> Result<(), DisputeError> {
        submitted_by.require_auth();
        require_not_paused(&env)?;

        let dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        if dispute.status != DisputeStatus::Open && dispute.status != DisputeStatus::Voting {
            return Err(DisputeError::VotingClosed);
        }

        if submitted_by != dispute.client && submitted_by != dispute.freelancer {
            return Err(DisputeError::InvalidParty);
        }

        let mut evidence: Vec<EvidenceRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Evidence(dispute_id))
            .unwrap_or(Vec::new(&env));

        // Check if evidence count has reached the cap
        if evidence.len() >= MAX_EVIDENCE_PER_DISPUTE {
            return Err(DisputeError::Unauthorized);
        }

        // Check if the evidence hash already exists
        for existing in evidence.iter() {
            if existing.evidence_hash == evidence_hash {
                return Err(DisputeError::Unauthorized);
            }
        }

        let record = EvidenceRecord {
            submitted_by: submitted_by.clone(),
            evidence_hash: evidence_hash.clone(),
            ledger: env.ledger().sequence(),
        };

        evidence.push_back(record);
        env.storage()
            .persistent()
            .set(&DataKey::Evidence(dispute_id), &evidence);
        bump_evidence_ttl(&env, dispute_id);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("evidence")),
            (dispute_id, submitted_by, evidence_hash, env.ledger().sequence()),
        );

        Ok(())
    }

    /// Get all evidence submitted for a dispute.
    pub fn get_evidence(env: Env, dispute_id: u64) -> Vec<EvidenceRecord> {
        let evidence = env
            .storage()
            .persistent()
            .get(&DataKey::Evidence(dispute_id))
            .unwrap_or(Vec::new(&env));
        if !evidence.is_empty() {
            bump_evidence_ttl(&env, dispute_id);
        }
        evidence
    }

    /// Get all arbitrators (voters) who have voted on a dispute.
    pub fn get_arbitrators(env: Env, dispute_id: u64) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Voters(dispute_id))
            .unwrap_or(Vec::<Address>::new(&env))
    }

    /// Get the assigned arbitrators for a dispute (those assigned via assign_arbitrators).
    /// This is different from get_arbitrators which returns voters who have actually cast votes.
    pub fn get_assigned_arbitrators(env: Env, dispute_id: u64) -> Vec<Address> {
        let arbitrators = env
            .storage()
            .persistent()
            .get(&DataKey::Arbitrators(dispute_id))
            .unwrap_or(Vec::<Address>::new(&env));
        if !arbitrators.is_empty() {
            bump_arbitrators_ttl(&env, dispute_id);
        }
        arbitrators
    }

    /// Get the DisputeTally for O(1) access to vote weights and counts.
    /// This is the authoritative source for weighted voting results.
    pub fn get_dispute_tally(env: Env, dispute_id: u64) -> Result<DisputeTally, DisputeError> {
        let tally = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeTally(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_tally_ttl(&env, dispute_id);
        Ok(tally)
    }

    /// Finalize the verdict for a dispute using O(1) tally accumulator.
    /// This function reads the pre-computed DisputeTally and determines the winner
    /// without iterating over individual votes, ensuring constant-time complexity
    /// regardless of the number of arbitrators (up to MAX_ARBITRATORS = 7).
    ///
    /// This is semantically equivalent to resolve_dispute but explicitly demonstrates
    /// the O(1) tally-based approach for issue #661.
    pub fn finalize_verdict(env: Env, dispute_id: u64) -> Result<DisputeStatus, DisputeError> {
        // Finalize verdict is just an alias for resolve_dispute with explicit O(1) semantics
        Self::resolve_dispute(env, dispute_id)
    }

    /// Get total dispute count.
    pub fn get_dispute_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::DisputeCount)
            .unwrap_or(0)
    }

    /// Delegate vote rights for a specific job's dispute to another address.
    /// The owner retains token ownership; only the right to cast the vote is transferred.
    /// The delegation is scoped to a single job so the delegate cannot vote on other jobs.
    pub fn delegate_vote(
        env: Env,
        owner: Address,
        delegate: Address,
        job_id: u64,
    ) -> Result<(), DisputeError> {
        owner.require_auth();
        require_not_paused(&env)?;

        if owner == delegate {
            return Err(DisputeError::InvalidParty);
        }

        // Reject if the owner already set up a delegation for this job.
        let del_key = DataKey::VoteDelegation(owner.clone(), job_id);
        if env.storage().persistent().has(&del_key) {
            return Err(DisputeError::AlreadyDelegated);
        }

        // If a dispute already exists for this job, ensure voting is still open and
        // the delegate hasn't voted yet.
        if let Some(dispute_id) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::JobDispute(job_id))
        {
            if let Some(dispute) = env
                .storage()
                .persistent()
                .get::<DataKey, Dispute>(&DataKey::Dispute(dispute_id))
            {
                if dispute.status != DisputeStatus::Open && dispute.status != DisputeStatus::Voting
                {
                    return Err(DisputeError::VotingClosed);
                }

                // Reject if the delegate already cast a vote on this dispute.
                let voted_key = DataKey::HasVoted(dispute_id, delegate.clone());
                if env.storage().persistent().has(&voted_key) {
                    return Err(DisputeError::DelegateAlreadyVoted);
                }

                // Reject if the owner already voted directly.
                let owner_voted_key = DataKey::HasVoted(dispute_id, owner.clone());
                if env.storage().persistent().has(&owner_voted_key) {
                    return Err(DisputeError::AlreadyVoted);
                }
            }
        }

        env.storage().persistent().set(&del_key, &delegate);
        bump_vote_delegation_ttl(&env, &owner, job_id);

        let owner_key = DataKey::DelegationOwner(delegate.clone(), job_id);
        env.storage().persistent().set(&owner_key, &owner);
        bump_delegation_owner_ttl(&env, &delegate, job_id);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("delegated")),
            (owner, delegate, job_id),
        );

        Ok(())
    }

    /// Revoke a previously granted vote delegation for a job, provided the delegate
    /// has not yet cast a vote on the associated dispute.
    pub fn revoke_delegation(env: Env, owner: Address, job_id: u64) -> Result<(), DisputeError> {
        owner.require_auth();
        require_not_paused(&env)?;

        let del_key = DataKey::VoteDelegation(owner.clone(), job_id);
        let delegate: Address = env
            .storage()
            .persistent()
            .get(&del_key)
            .ok_or(DisputeError::DelegationNotFound)?;

        // Block revocation once the delegate has already voted.
        if let Some(dispute_id) = env
            .storage()
            .persistent()
            .get::<DataKey, u64>(&DataKey::JobDispute(job_id))
        {
            let voted_key = DataKey::HasVoted(dispute_id, delegate.clone());
            if env.storage().persistent().has(&voted_key) {
                return Err(DisputeError::DelegateAlreadyVoted);
            }
        }

        env.storage().persistent().remove(&del_key);

        let owner_key = DataKey::DelegationOwner(delegate.clone(), job_id);
        env.storage().persistent().remove(&owner_key);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("del_revkd")),
            (owner, delegate, job_id),
        );

        Ok(())
    }

    /// Return the current delegate for an owner / job pair, if one exists.
    pub fn get_delegation(env: Env, owner: Address, job_id: u64) -> Option<Address> {
        let del_key = DataKey::VoteDelegation(owner.clone(), job_id);
        let delegate: Option<Address> = env.storage().persistent().get(&del_key);
        if delegate.is_some() {
            bump_vote_delegation_ttl(&env, &owner, job_id);
        }
        delegate
    }

    /// Check if a voter is excluded from voting on a specific dispute.
    pub fn is_excluded_voter(env: Env, dispute_id: u64, voter: Address) -> bool {
        let dispute_result: Option<Dispute> = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id));

        match dispute_result {
            Some(dispute) => dispute.excluded_voters.contains(&voter),
            None => false,
        }
    }

    /// Add an arbitrator to the pool (admin only).
    pub fn add_arbitrator(env: Env, admin: Address, arbitrator: Address) -> Result<(), DisputeError> {
        admin.require_auth();
        require_not_paused(&env)?;
        require_admin(&env, &admin)?;

        let mut pool: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::ArbitratorPool)
            .unwrap_or(Vec::new(&env));

        if pool.contains(&arbitrator) {
            return Err(DisputeError::DuplicateArbitrator);
        }

        pool.push_back(arbitrator.clone());
        env.storage().instance().set(&DataKey::ArbitratorPool, &pool);
        bump_dispute_count_ttl(&env);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("arb_added")),
            (admin, arbitrator),
        );

        Ok(())
    }

    /// Remove an arbitrator from the pool (admin only).
    /// Also excludes them from voting on any open disputes they were assigned to.
    pub fn remove_arbitrator(env: Env, admin: Address, arbitrator: Address) -> Result<(), DisputeError> {
        admin.require_auth();
        require_not_paused(&env)?;
        require_admin(&env, &admin)?;

        let pool: Vec<Address> = env
            .storage()
            .instance()
            .get(&DataKey::ArbitratorPool)
            .unwrap_or(Vec::new(&env));

        let mut new_pool = Vec::<Address>::new(&env);
        let mut removed = false;

        for addr in pool.iter() {
            if addr != arbitrator {
                new_pool.push_back(addr);
            } else {
                removed = true;
            }
        }

        if removed {
            env.storage().instance().set(&DataKey::ArbitratorPool, &new_pool);
            bump_dispute_count_ttl(&env);

            // Revoke the arbitrator's voting rights on all open disputes they were assigned to.
            let dispute_count: u64 = env
                .storage()
                .instance()
                .get(&DataKey::DisputeCount)
                .unwrap_or(0);

            for id in 1..=dispute_count {
                let mut dispute: Dispute = match env
                    .storage()
                    .persistent()
                    .get(&DataKey::Dispute(id))
                {
                    Some(d) => d,
                    None => continue,
                };

                // Only modify open disputes where the arbitrator is assigned and not yet excluded
                if dispute.status == DisputeStatus::Open
                    && dispute.assigned_arbitrators.contains(&arbitrator)
                    && !dispute.excluded_voters.contains(&arbitrator)
                {
                    dispute.excluded_voters.push_back(arbitrator.clone());
                    env.storage()
                        .persistent()
                        .set(&DataKey::Dispute(id), &dispute);
                    bump_dispute_ttl(&env, id);
                }
            }

            env.events().publish(
                (symbol_short!("dispute"), symbol_short!("arb_rmvd")),
                (admin, arbitrator),
            );
        }

        Ok(())
    }

    /// Get the current arbitrator pool.
    pub fn get_arbitrator_pool(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&DataKey::ArbitratorPool)
            .unwrap_or(Vec::new(&env))
    }

    /// Retry the escrow callback for a dispute whose resolution previously failed.
    ///
    /// Permissionless and idempotent: safe to call repeatedly until escrow accepts.
    /// Returns the current dispute status so callers can see whether the retry succeeded.
    pub fn retry_escrow_callback(env: Env, dispute_id: u64) -> Result<DisputeStatus, DisputeError> {
        require_not_paused(&env)?;

        let mut dispute: Dispute = env
            .storage()
            .persistent()
            .get(&DataKey::Dispute(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_dispute_ttl(&env, dispute_id);

        if dispute.status != DisputeStatus::ResolutionFailed {
            return Err(DisputeError::AlreadyResolved);
        }

        let resolution: DisputeResolution = env
            .storage()
            .persistent()
            .get(&DataKey::PendingResolution(dispute_id))
            .ok_or(DisputeError::DisputeNotFound)?;
        bump_pending_resolution_ttl(&env, dispute_id);

        let escrow_addr: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .ok_or(DisputeError::NotInitialized)?;

        let escrow_ok = matches!(
            env.try_invoke_contract::<(), soroban_sdk::Error>(
                &escrow_addr,
                &Symbol::new(&env, "resolve_dispute_callback"),
                vec![&env, dispute.job_id.into_val(&env), resolution.clone().into_val(&env)],
            ),
            Ok(Ok(_))
        );

        if !escrow_ok {
            env.events().publish(
                (symbol_short!("dispute"), Symbol::new(&env, "escrow_fail")),
                (dispute_id, dispute.job_id),
            );
            return Ok(DisputeStatus::ResolutionFailed);
        }

        dispute.status = resolution_to_status(&resolution);

        env.storage()
            .persistent()
            .remove(&DataKey::PendingResolution(dispute_id));

        env.storage().persistent().set(
            &DataKey::LastDisputeClosedAt(dispute.job_id),
            &env.ledger().timestamp(),
        );
        bump_last_dispute_closed_ttl(&env, dispute.job_id);

        env.storage().persistent().set(
            &DataKey::LastDisputeLedger(dispute.client.clone(), dispute.freelancer.clone()),
            &env.ledger().timestamp(),
        );
        bump_last_dispute_ledger_ttl(&env, &dispute.client, &dispute.freelancer);

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(dispute_id), &dispute);
        bump_dispute_ttl(&env, dispute_id);

        env.events().publish(
            (symbol_short!("dispute"), symbol_short!("resolved")),
            (
                dispute_id,
                dispute.status.clone(),
                dispute.job_id,
                dispute.client.clone(),
                dispute.freelancer.clone(),
                resolution,
            ),
        );

        Ok(dispute.status.clone())
    }
}

/// Returns the median client_bps from all SplitAward votes using insertion sort.
/// Falls back to 5000 (50/50) when no SplitAward votes are present.
fn compute_median_bps(env: &Env, votes: &Vec<Vote>) -> u32 {
    let mut sorted = Vec::<u32>::new(env);
    for vote in votes.iter() {
        if let VoteChoice::SplitAward(client_bps, _) = vote.choice {
            let mut pos = sorted.len();
            for i in 0..sorted.len() {
                if client_bps <= sorted.get(i).unwrap() {
                    pos = i;
                    break;
                }
            }
            sorted.insert(pos, client_bps);
        }
    }
    let n = sorted.len();
    if n == 0 {
        return 5_000;
    }
    sorted.get(n / 2).unwrap()
}

fn resolution_to_status(resolution: &DisputeResolution) -> DisputeStatus {
    match resolution {
        DisputeResolution::ClientWins => DisputeStatus::ResolvedForClient,
        DisputeResolution::FreelancerWins => DisputeStatus::ResolvedForFreelancer,
        DisputeResolution::RefundBoth => DisputeStatus::RefundedBoth,
        DisputeResolution::RefundSplit(pct) => DisputeStatus::RefundSplit(*pct),
        DisputeResolution::SplitAward(bps) => DisputeStatus::SplitAward(*bps),
        DisputeResolution::MaliciousFiling => DisputeStatus::MaliciousDisputeFiling,
        DisputeResolution::Escalate => DisputeStatus::Escalated,
    }
}

fn internal_resolve(
    env: &Env,
    dispute_id: u64,
    dispute: &mut Dispute,
    escrow_addr: &Address,
    force: bool,
) -> Result<DisputeStatus, DisputeError> {
    if dispute.status == DisputeStatus::ResolvedForClient
        || dispute.status == DisputeStatus::ResolvedForFreelancer
        || dispute.status == DisputeStatus::RefundedBoth
        || matches!(dispute.status, DisputeStatus::RefundSplit(_))
        || matches!(dispute.status, DisputeStatus::SplitAward(_))
        || dispute.status == DisputeStatus::Escalated
        || dispute.status == DisputeStatus::MaliciousDisputeFiling
        || dispute.status == DisputeStatus::ResolutionFailed
    {
        return Err(DisputeError::AlreadyResolved);
    }

    let sa = dispute.votes_for_split_award;
    let total_votes = dispute.votes_for_client
        + dispute.votes_for_freelancer
        + dispute.votes_for_refund_split
        + dispute.votes_for_malicious
        + sa;
    if !force && total_votes < dispute.min_votes {
        return Err(DisputeError::NotEnoughVotes);
    }

    // ── Supermajority check: MaliciousFiling requires 4 out of every 5 votes ─────
    // votes_for_malicious * NUM >= total_votes * DENOM  ↔  ≥ 80 % of all votes
    let is_malicious_supermajority = total_votes >= MALICIOUS_FILING_MIN_VOTES
        && dispute
            .votes_for_malicious
            .saturating_mul(MALICIOUS_FILING_SUPERMAJORITY_DENOM)
            >= total_votes.saturating_mul(MALICIOUS_FILING_SUPERMAJORITY_NUM);

    if is_malicious_supermajority {
        dispute.status = DisputeStatus::MaliciousDisputeFiling;

        // Notify escrow: slash full stake of initiator to treasury.
        let escrow_ok = matches!(
            env.try_invoke_contract::<(), soroban_sdk::Error>(
                escrow_addr,
                &Symbol::new(env, "resolve_dispute_callback"),
                vec![
                    env,
                    dispute.job_id.into_val(env),
                    DisputeResolution::MaliciousFiling.into_val(env),
                ],
            ),
            Ok(Ok(_))
        );

        if !escrow_ok {
            dispute.status = DisputeStatus::ResolutionFailed;
            env.storage()
                .persistent()
                .set(&DataKey::PendingResolution(dispute_id), &DisputeResolution::MaliciousFiling);
            bump_pending_resolution_ttl(env, dispute_id);
            env.storage()
                .persistent()
                .set(&DataKey::Dispute(dispute_id), &*dispute);
            bump_dispute_ttl(env, dispute_id);
            env.events().publish(
                (symbol_short!("dispute"), Symbol::new(env, "escrow_fail")),
                (dispute_id, dispute.job_id),
            );
            return Ok(DisputeStatus::ResolutionFailed);
        }

        // Cross-contract call to reputation contract: apply MaliciousFiling penalty.
        if let Some(reputation_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::ReputationContract)
        {
            // Import DisputeOutcome inline so we can pass it to the reputation contract.
            mod reputation_types {
                use soroban_sdk::contracttype;
                #[contracttype]
                #[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
                #[repr(u32)]
                pub enum DisputeOutcome {
                    Won = 0,
                    Lost = 1,
                    MaliciousFiling = 2,
                }
            }

            let outcome = reputation_types::DisputeOutcome::MaliciousFiling;
            let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
                &reputation_contract,
                &Symbol::new(env, "apply_dispute_outcome"),
                vec![
                    env,
                    dispute.initiator.clone().into_val(env),
                    outcome.into_val(env),
                ],
            );
        }

        // Persist state before emitting events.
        env.storage()
            .persistent()
            .set(&DataKey::LastDisputeClosedAt(dispute.job_id), &env.ledger().timestamp());
        bump_last_dispute_closed_ttl(env, dispute.job_id);

        env.storage().persistent().set(
            &DataKey::LastDisputeLedger(dispute.client.clone(), dispute.freelancer.clone()),
            &env.ledger().timestamp(),
        );
        bump_last_dispute_ledger_ttl(env, &dispute.client, &dispute.freelancer);

        env.storage()
            .persistent()
            .set(&DataKey::Dispute(dispute_id), &*dispute);
        bump_dispute_ttl(env, dispute_id);

        // Emit dedicated MaliciousDisputeResolved event.
        env.events().publish(
            (symbol_short!("dispute"), Symbol::new(env, "malicious_rslvd")),
            (dispute_id, dispute.job_id, dispute.initiator.clone()),
        );

        return Ok(dispute.status.clone());
    }

    // ── Normal resolution path ────────────────────────────────────────────────
    if dispute.votes_for_client > dispute.votes_for_freelancer
        && dispute.votes_for_client > dispute.votes_for_refund_split
        && dispute.votes_for_client > dispute.votes_for_malicious
    {
        dispute.status = DisputeStatus::ResolvedForClient;
    } else if dispute.votes_for_freelancer > dispute.votes_for_client
        && dispute.votes_for_freelancer > dispute.votes_for_refund_split
        && dispute.votes_for_freelancer > dispute.votes_for_malicious
    {
        dispute.status = DisputeStatus::ResolvedForFreelancer;
    } else if sa > dispute.votes_for_client
        && sa > dispute.votes_for_freelancer
        && sa > dispute.votes_for_refund_split
    {
        let stored_votes: Vec<Vote> = env
            .storage()
            .persistent()
            .get(&DataKey::Votes(dispute_id))
            .unwrap_or(Vec::new(env));
        let median = compute_median_bps(env, &stored_votes);
        dispute.status = DisputeStatus::SplitAward(median);
    } else if dispute.votes_for_refund_split > dispute.votes_for_client
        && dispute.votes_for_refund_split > dispute.votes_for_freelancer
        && dispute.votes_for_refund_split > dispute.votes_for_malicious
    {
        let avg = dispute.refund_split_sum / dispute.votes_for_refund_split as u64;
        dispute.status = DisputeStatus::RefundSplit(avg as u32);
    } else if dispute.tally.client_weight > 0
        && dispute.tally.client_weight == dispute.tally.freelancer_weight
    {
        // Exact tie between client and freelancer — resolve as 50/50 split.
        dispute.status = DisputeStatus::RefundSplit(50);

        env.storage().persistent().set(
            &DataKey::SplitRatio(dispute_id),
            &(50u32, 50u32),
        );
    } else {
        // Tie-break logic (applies if votes are tied OR if total_votes is 0 in force mode)
        match dispute.tie_break_method {
            TieBreakMethod::FavorClient => dispute.status = DisputeStatus::ResolvedForClient,
            TieBreakMethod::FavorFreelancer => {
                dispute.status = DisputeStatus::ResolvedForFreelancer
            }
            TieBreakMethod::RefundBoth => dispute.status = DisputeStatus::RefundedBoth,
            TieBreakMethod::Escalate => dispute.status = DisputeStatus::Escalated,
        }
    }

    let resolution = match dispute.status {
        DisputeStatus::ResolvedForClient => DisputeResolution::ClientWins,
        DisputeStatus::ResolvedForFreelancer => DisputeResolution::FreelancerWins,
        DisputeStatus::RefundedBoth => DisputeResolution::RefundBoth,
        DisputeStatus::RefundSplit(pct) => DisputeResolution::RefundSplit(pct),
        DisputeStatus::SplitAward(bps) => DisputeResolution::SplitAward(bps),
        _ => DisputeResolution::Escalate,
    };

    // Only invoke the escrow callback if the dispute has a concrete resolution.
    if resolution != DisputeResolution::Escalate {
        let escrow_ok = matches!(
            env.try_invoke_contract::<(), soroban_sdk::Error>(
                escrow_addr,
                &Symbol::new(env, "resolve_dispute_callback"),
                vec![env, dispute.job_id.into_val(env), resolution.clone().into_val(env)],
            ),
            Ok(Ok(_))
        );

        if !escrow_ok {
            dispute.status = DisputeStatus::ResolutionFailed;
            env.storage()
                .persistent()
                .set(&DataKey::PendingResolution(dispute_id), &resolution);
            bump_pending_resolution_ttl(env, dispute_id);
            env.storage()
                .persistent()
                .set(&DataKey::Dispute(dispute_id), &*dispute);
            bump_dispute_ttl(env, dispute_id);
            env.events().publish(
                (symbol_short!("dispute"), Symbol::new(env, "escrow_fail")),
                (dispute_id, dispute.job_id),
            );
            return Ok(DisputeStatus::ResolutionFailed);
        }

        // Slash the losing party's reputation score.
        if let Some(reputation_contract) = env
            .storage()
            .instance()
            .get::<DataKey, Address>(&DataKey::ReputationContract)
        {
            let loser = match resolution {
                DisputeResolution::ClientWins => dispute.freelancer.clone(),
                DisputeResolution::FreelancerWins => dispute.client.clone(),
                DisputeResolution::RefundBoth => dispute.initiator.clone(),
                DisputeResolution::RefundSplit(_) => dispute.initiator.clone(),
                DisputeResolution::SplitAward(_) => dispute.initiator.clone(),
                DisputeResolution::Escalate => unreachable!(),
                DisputeResolution::MaliciousFiling => unreachable!(),
            };

            let slash_bps: u32 = env
                .storage()
                .instance()
                .get(&DataKey::ReputationSlashBps)
                .unwrap_or(DEFAULT_REPUTATION_SLASH_BPS);

            let current_score = env
                .try_invoke_contract::<reputation::UserReputation, soroban_sdk::Error>(
                    &reputation_contract,
                    &Symbol::new(env, "get_reputation"),
                    vec![env, loser.clone().into_val(env)],
                )
                .ok()
                .and_then(|r| r.ok())
                .map(|r| r.total_score)
                .unwrap_or(0);

            let mut slash_amount: u64 = (current_score.saturating_mul(slash_bps as u64)) / 10_000;
            if slash_amount == 0 && current_score > 0 {
                slash_amount = 1;
            }

            let reason = String::from_str(env, "dispute_lost");

            let _ = env.try_invoke_contract::<(), soroban_sdk::Error>(
                &reputation_contract,
                &Symbol::new(env, "slash_reputation"),
                vec![
                    env,
                    loser.clone().into_val(env),
                    dispute.job_id.into_val(env),
                    slash_amount.into_val(env),
                    reason.into_val(env),
                ],
            );

            env.events().publish(
                (symbol_short!("dispute"), Symbol::new(env, "reput_slashed")),
                (dispute.job_id, loser, slash_amount),
            );
        }
    }

    env.storage().persistent().set(
        &DataKey::LastDisputeClosedAt(dispute.job_id),
        &env.ledger().timestamp(),
    );
    bump_last_dispute_closed_ttl(env, dispute.job_id);

    env.storage().persistent().set(
        &DataKey::LastDisputeLedger(dispute.client.clone(), dispute.freelancer.clone()),
        &env.ledger().timestamp(),
    );
    bump_last_dispute_ledger_ttl(env, &dispute.client, &dispute.freelancer);

    env.storage()
        .persistent()
        .set(&DataKey::Dispute(dispute_id), &*dispute);
    bump_dispute_ttl(env, dispute_id);

    env.events().publish(
        (symbol_short!("dispute"), symbol_short!("resolved")),
        (
            dispute_id,
            dispute.status.clone(),
            dispute.job_id,
            dispute.client.clone(),
            dispute.freelancer.clone(),
            resolution,
        ),
    );

    Ok(dispute.status.clone())
}

#[cfg(test)]
mod test;
