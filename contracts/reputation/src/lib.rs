#![no_std]
//! # Reputation contract
//!
//! ## Two distinct measures of standing (issue #1172)
//!
//! This contract exposes **two independent ladders**. They are computed from
//! different inputs, answer different questions, and routinely disagree — a user
//! can sit at the top of one and the bottom of the other. They are deliberately
//! named apart so they are not mistaken for the same thing:
//!
//! | | **Rating tier** ([`ReputationTier`]) | **Score badge** ([`ScoreBadge`]) |
//! |---|---|---|
//! | Read via | [`ReputationContract::get_tier`] | [`ReputationContract::get_score_badge`] |
//! | Computed from | `average_rating` = decayed `total_score / total_weight × 100`, multiplied by the user's stake multiplier and capped at 10,000 | raw decayed `total_score` = Σ(`rating` × per-review `stake_weight`) |
//! | Grows with review count? | **No** — it is an average, so a 5★ user with 1 review ranks alongside a 5★ user with 100 | **Yes** — every additional review and every additional staked unit pushes it up without bound |
//! | Rungs | None / Bronze (≥100) / Silver (≥300) / Gold (≥500) / Platinum (≥700) | None / Rising (≥100) / Established (≥500) / Elite (≥2000) |
//! | Answers | "how well does this user perform?" | "how much proven, stake-backed history does this user have?" |
//!
//! Consequences worth knowing before integrating:
//!
//! - The two ladders have **different numbers of rungs and different thresholds**;
//!   they are not parallel and are not meant to be. There is no "Platinum" score
//!   badge, and a `Gold` rating tier implies nothing about the score badge.
//! - The ladders share only the word "reputation". Do not map one onto the other,
//!   and do not present them to users as one combined level.
//! - [`ReputationContract::get_badges`] is a *third* thing again: it returns the
//!   **rating tiers** ([`AwardedBadge::badge_type`] is a [`ReputationTier`]) that a
//!   user has passed through and permanently keeps, whereas the rating tier and the
//!   score badge are both computed live and can fall as reviews decay.
//! - Both ladders read decayed values, so both drift downward while a user is
//!   dormant.
//!
//! `get_badge` is retained as a backwards-compatible alias of `get_score_badge`.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, token, Address, Env, String,
    Symbol, Vec,
};
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
    pub struct TokenBalance {
        pub token: Address,
        pub total_amount: i128,
        pub funded_amount: i128,
    }

    #[contracttype]
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct Milestone {
        pub id: u32,
        pub description: String,
        pub amount: i128,
        pub status: MilestoneStatus,
        pub deadline: u64,
        pub token: Option<Address>,
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
        pub expiry_ledger: u32,
        pub token_balances: Vec<TokenBalance>,
    }

    #[soroban_sdk::contractclient(name = "EscrowContractClient")]
    #[allow(dead_code)]
    pub trait EscrowInterface {
        fn get_job(env: soroban_sdk::Env, job_id: u64) -> Job;
    }
}

use escrow::{EscrowContractClient, JobStatus};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ReputationError {
    InvalidRating = 1,
    AlreadyReviewed = 2,
    SelfReview = 3,
    UserNotFound = 4,
    JobNotCompleted = 5,
    NotJobParticipant = 6,
    JobNotFound = 7,
    Unauthorized = 8,
    NotInitialized = 9,
    InvalidDecayRate = 10,
    BelowMinStake = 11,
    RateLimitExceeded = 12,
    ContractPaused = 13,
    NotAdmin = 14,
    AlreadyReferred = 15,
    SelfReferral = 16,
    CircularReferral = 17,
    ReviewNotFound = 18,
    AppealWindowExpired = 19,
    AppealAlreadyExists = 20,
    AppealNotFound = 21,
    AppealAlreadyResolved = 22,
    AlreadyEndorsed = 23,
    // Rejected when a referral bonus is recorded with a timestamp in the future.
    // A future-dated bonus would keep `get_decay_factor` at `elapsed_seconds = 0`,
    // permanently exempting it from decay and inflating the score (issue #781).
    InvalidTimestamp = 24,
    // Rejected when a decay rate exceeds the configured maximum. An unbounded
    // decay rate (e.g. 99%) would wipe out the leaderboard in a single period
    // and make reputation meaningless (issue #783).
    DecayRateTooHigh = 25,
    /// Rejected when stake_weight is below the configured minimum stake weight.
    /// A zero-stake review carries vote weight of 1 even though the reviewer has
    /// no economic skin in the game; the minimum stake weight floor prevents this.
    StakeTooLow = 26,
    /// Rejected when `endorse` is called with `endorser == target`. Without this
    /// guard, a user with an established rating could add themselves as their
    /// own skill endorser and inflate their own `get_skill_score` (issue #987).
    SelfEndorsement = 27,
    /// Rejected when `claim_stake` finds a positive `StakeBalance` but no
    /// recorded `StakeToken` for the reviewer (stake predates token tracking).
    StakeTokenNotFound = 28,
    /// Rejected when `claim_stake` is called before the lockup period has
    /// elapsed. Stake must remain locked for `STAKE_LOCKUP_SECONDS` after the
    /// first review that deposited it, preventing flash-loan-funded governance
    /// weight inflation where an attacker stakes, inflates reputation/governance
    /// weight, and immediately reclaims the stake in the same block (issue #exploit).
    StakeLockupActive = 29,
    /// Rejected when `set_stake_tiers` is called with more tiers than
    /// `MAX_STAKE_TIERS`. An unbounded tier list would make every call to
    /// `get_stake_multiplier` (on the hot path for `get_average_rating`,
    /// `get_score_badge`, and leaderboard updates) progressively more expensive,
    /// mirroring the cap pattern used for `MAX_ENDORSERS_COUNTED`,
    /// `MAX_REVIEWS_COUNTED`, and `MAX_REVIEWS_PER_REVIEWEE_WINDOW` (issue #1177).
    TooManyStakeTiers = 30,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Review {
    pub reviewer: Address,
    pub reviewee: Address,
    pub job_id: u64,
    pub rating: u32,
    pub comment: String,
    pub stake_weight: i128,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserReputation {
    pub user: Address,
    pub total_score: u64,
    pub total_weight: u64,
    pub review_count: u32,
    /// Unix **timestamp** (seconds) of the last score-changing event, from
    /// `env.ledger().timestamp()` — NOT a ledger sequence number. Renamed from
    /// `last_updated_ledger` to match what it actually holds (issue #899 review).
    pub last_updated_ts: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeTier {
    pub threshold: i128,
    pub multiplier: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserReputationWithReferrer {
    pub user: Address,
    pub referrer: Option<Address>,
    pub total_score: u64,
    pub total_weight: u64,
    pub review_count: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralBonusRecord {
    pub amount: u64,
    pub weight: u64,
    pub timestamp: u64,
}

/// **Rating tier** — a user's standing on the *quality* ladder, derived from
/// `average_rating` (see [`ReputationContract::get_tier`]). Independent of how many
/// reviews a user has: it is an average, so one glowing review can reach the same
/// tier as a hundred. Not to be confused with [`ScoreBadge`], the *volume* ladder —
/// see the module docs for the full comparison (issue #1172).
///
/// Thresholds on `average_rating` (0–10,000): Bronze ≥ 100, Silver ≥ 300,
/// Gold ≥ 500, Platinum ≥ 700.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ReputationTier {
    None = 0,
    Bronze = 1,
    Silver = 2,
    Gold = 3,
    Platinum = 4,
}

/// **Score badge** — a user's standing on the *volume* ladder, derived from the raw
/// decayed `total_score` (see [`ReputationContract::get_score_badge`]). It rises
/// with every additional review and with the stake backing those reviews, so it
/// measures accumulated, stake-backed history rather than quality.
///
/// Deliberately named apart from [`ReputationTier`] (issue #1172): the two ladders
/// use different inputs and thresholds and frequently disagree. The rungs are
/// intentionally not parallel to the rating tiers — there is no Platinum here, and
/// `Elite` is the top of a three-rung ladder.
///
/// Thresholds on the decayed `total_score`: Rising ≥ 100, Established ≥ 500,
/// Elite ≥ 2000. Discriminants match the previous `Badge` enum, so encoded values
/// are unchanged: `Rising` was `Bronze`, `Established` was `Silver`, `Elite` was
/// `Gold`.
#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum ScoreBadge {
    None = 0,
    Rising = 1,
    Established = 2,
    Elite = 3,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum DisputeOutcome {
    Won = 0,
    Lost = 1,
    MaliciousFiling = 2,
}

/// A **rating tier** the user has reached at least once, kept permanently.
///
/// Despite the name, this has nothing to do with [`ScoreBadge`]: `badge_type` is a
/// [`ReputationTier`], awarded by `submit_review` when a user first crosses a tier
/// threshold. Unlike the two live ladders it never falls back — decay can lower a
/// user's current tier and score badge while these awarded records stay (issue
/// #1172).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AwardedBadge {
    /// The rating tier that was reached — **not** a [`ScoreBadge`].
    pub badge_type: ReputationTier,
    pub awarded_at: u64,
}

#[contracttype]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum AppealStatus {
    Pending = 0,
    Dismissed = 1,
    ReviewRemoved = 2,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewAppeal {
    pub reviewer: Address,
    pub reviewee: Address,
    pub job_id: u64,
    pub reason: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub status: AppealStatus,
}

/// Privileged actions that can be proposed and approved through the multi-sig flow.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum AdminAction {
    Pause,
    Unpause,
    SetMinStake(i128),
    SetRateLimit(u32),
    SetToken(Address),
    SetReferralBonus(u64),
    SetDecayRate(u32),
    SlashStake(Address, u64, u64), // loser, job_id, amount
    AddSigner(Address),
    RemoveSigner(Address),
    ChangeThreshold(u32),
    RotateSigner(Address, Address),
    SetStakeTiers(Vec<StakeTier>),
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

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferralStats {
    pub total_referrals: u32,
    pub earned_bonus: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
enum DataKey {
    Reputation(Address),
    Reviews(Address),
    ReviewExists(Address, Address, u64),
    Badges(Address),
    Admin, // Legacy
    DecayRate,
    // Configurable upper bound for `DecayRate`, settable by a super-admin up to
    // `MAX_DECAY_RATE_HARD_CEILING`. Falls back to `MAX_DECAY_RATE` when unset.
    MaxDecayRate,
    MinStake,
    RateLimit,
    LastReviewLedger(Address),
    Token,
    Paused,
    Referrer(Address),
    ReferralStats(Address),
    BonusPaid(Address),
    ReferralBonus,
    ReferralBonusList(Address),
    MultiSigSigners,
    MultiSigThreshold,
    MultiSigProposal(u64),
    MultiSigProposalCount,
    Leaderboard,
    StakeBalance(Address),
    /// The token address a reviewer's current `StakeBalance` is denominated in,
    /// so it can be transferred back correctly on `claim_stake`.
    StakeToken(Address),
    /// The ledger timestamp at which the reviewer first staked (i.e. submitted
    /// their first review). Used by `claim_stake` to enforce `STAKE_LOCKUP_SECONDS`.
    /// Reset to the current timestamp whenever additional stake is added so that
    /// all stake (including incremental deposits) must age past the lockup.
    StakeLockupTs(Address),
    ReviewAppeal(Address, Address, u64),
    DisputeContract,
    Endorsement(Address, String, Address),
    SkillEndorsers(Address, String),
    StakeTiers,
    /// Admin-configurable minimum stake weight. Reviews with stake_weight below
    /// this value are rejected even when the economic min_stake allows zero stakes.
    MinStakeWeight,
    /// Marks a user as banned by an admin. Banned users are excluded from
    /// leaderboard queries.
    BannedUser(Address),
    /// Tracks (window_start_ledger, count) of reviews received by this
    /// reviewee within the current rate-limit window (issue #1117).
    RevieweeReviewWindow(Address),
}

fn require_not_paused(env: &Env) -> Result<(), ReputationError> {
    if env
        .storage()
        .instance()
        .get(&DataKey::Paused)
        .unwrap_or(false)
    {
        return Err(ReputationError::ContractPaused);
    }
    Ok(())
}

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

/// Effective upper bound for the reputation decay rate: the super-admin
/// configured value if present, otherwise the `MAX_DECAY_RATE` default.
fn effective_max_decay_rate(env: &Env) -> u32 {
    env.storage()
        .instance()
        .get(&DataKey::MaxDecayRate)
        .unwrap_or(MAX_DECAY_RATE)
}

const MIN_REVIEW_STAKE_DEFAULT: i128 = 10_000_000; // 1.0 unit (7 decimals)
const RATE_LIMIT_LEDGERS_DEFAULT: u32 = 120; // ~10 minutes
/// Hard floor on the stake_weight used as reputation vote weight.
/// Prevents zero-weight reviews from gaining weight=1 via the fallback path.
pub const MIN_STAKE_WEIGHT: u64 = 1;

/// Minimum lockup duration in seconds before staked tokens can be reclaimed.
/// 7 days (604_800 seconds). This ensures that reputation weight established by
/// a stake cannot be flash-loaned: the economic cost must be borne for at least
/// this period, making governance manipulation via rapid stake/unstake
/// economically meaningful rather than free (issue #exploit).
pub const STAKE_LOCKUP_SECONDS: u64 = 7 * 24 * 60 * 60; // 604_800

/// Per-review cap on the stake weight credited toward `total_score` and
/// `total_weight`. Any stake_weight above this threshold is clamped before it
/// is multiplied by the star rating and added to the reviewee's reputation.
/// This prevents a single self-dealt review with an arbitrarily large
/// `stake_weight` from dominating `total_score` and thereby inflating
/// governance voting power via `get_gov_weight` (issue #exploit).
///
/// Set to 1_000 units at 7 decimals = 10_000_000_000 stroops. Reviewers who
/// want more governance influence must earn it across multiple distinct
/// counterparties rather than from a single inflated stake.
pub const MAX_STAKE_WEIGHT_PER_REVIEW: u64 = 1_000 * 10_000_000; // 10_000_000_000

/// Absolute cap on the `score` value returned by `get_gov_weight`. Defense-in-
/// depth against any future bypass of the per-review cap. Equals five 5-star
/// max-weight reviews times the maximum counted reviews (200), so a legitimately
/// excellent user is never capped in practice.
pub const MAX_GOV_WEIGHT: u64 = 5 * MAX_STAKE_WEIGHT_PER_REVIEW * 200; // 10_000_000_000_000

const DEFAULT_REFERRAL_BONUS: u64 = 5; // Equivalates to a 5-star review bonus
/// Weight used when crediting referral bonus to reputation (not min review stake).
const REFERRAL_BONUS_REPUTATION_WEIGHT: u64 = 1;
const ONE_YEAR_IN_SECONDS: u64 = 31_536_000;/// Default upper bound for the annual reputation `decay_rate` (percent per year).
/// 20% keeps decay meaningful without destroying accumulated reputation in a
/// single period. The super-admin can raise this via `set_max_decay_rate` up to
/// `MAX_DECAY_RATE_HARD_CEILING` (issue #783).
const MAX_DECAY_RATE: u32 = 20;
/// Absolute ceiling the configurable maximum decay rate can never exceed.
const MAX_DECAY_RATE_HARD_CEILING: u32 = 50;

const MIN_TTL_THRESHOLD: u32 = 50_000_000;
const MIN_TTL_EXTEND_TO: u32 = 50_000_000;
const APPEAL_GRACE_WINDOW_SECONDS: u64 = 72 * 60 * 60;
// Bounds how many endorsers `get_skill_score` sums over. Without a cap, the
// `SkillEndorsers` list (and therefore the loop's cost and the returned
// score) could grow without limit (issue #987).
const MAX_ENDORSERS_COUNTED: u32 = 30;
// Bounds how many Review/ReferralBonusRecord entries `get_decayed_totals`
// scores per user, following the same pattern as `MAX_ENDORSERS_COUNTED`.
// Without a cap, a griefer could spam low-cost completed jobs/reviews against
// a single target to grow their `Reviews(user)` vector without bound, making
// every read of that user's score (get_reputation, get_gov_weight, and
// dispute's is_eligible_voter) exceed the Soroban instruction budget and
// silently lock them out of governance/dispute-voting eligibility (issue
// #1117). Only the most recent entries are scored, since decay already makes
// older entries contribute the least to the total.
const MAX_REVIEWS_COUNTED: u32 = 200;
/// Complementary mitigation for issue #1117: caps how many reviews a single
/// reviewee can accumulate within one rate-limit window (the same window
/// used by the existing per-reviewer cooldown), slowing how fast a griefer
/// can grow a target's Reviews vector in the first place.
const MAX_REVIEWS_PER_REVIEWEE_WINDOW: u32 = 20;
// Bounds how many stake tiers `set_stake_tiers` accepts. `get_stake_multiplier`
// iterates the full tier list on every rating computation (hot path for
// `get_average_rating`, `get_score_badge`, and leaderboard updates), so an unbounded
// list would make those calls progressively more expensive (issue #1177).
const MAX_STAKE_TIERS: u32 = 10;

fn bump_reputation_ttl(env: &Env, user: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::Reputation(user.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_reviews_ttl(env: &Env, user: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::Reviews(user.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_review_exists_ttl(env: &Env, reviewer: &Address, reviewee: &Address, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::ReviewExists(reviewer.clone(), reviewee.clone(), job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_badges_ttl(env: &Env, user: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::Badges(user.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_review_appeal_ttl(env: &Env, reviewer: &Address, reviewee: &Address, job_id: u64) {
    env.storage().persistent().extend_ttl(
        &DataKey::ReviewAppeal(reviewer.clone(), reviewee.clone(), job_id),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_endorsement_ttl(env: &Env, target: &Address, skill: &String, endorser: &Address) {
    env.storage().persistent().extend_ttl(
        &DataKey::Endorsement(target.clone(), skill.clone(), endorser.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
    env.storage().persistent().extend_ttl(
        &DataKey::SkillEndorsers(target.clone(), skill.clone()),
        MIN_TTL_THRESHOLD,
        MIN_TTL_EXTEND_TO,
    );
}

fn bump_instance_ttl(env: &Env) {
    env.storage()
        .instance()
        .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
}

pub fn apply_lazy_decay(env: &Env, rep: &mut UserReputation) {
    env.storage()
        .instance()
        .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
    let decay_rate: u32 = env
        .storage()
        .instance()
        .get(&DataKey::DecayRate)
        .unwrap_or(0);

    let current_ts = env.ledger().timestamp() as u32;
    if decay_rate == 0 || decay_rate >= 100 {
        rep.last_updated_ts = current_ts;
        return;
    }

    let last_ts = rep.last_updated_ts as u64;
    let now_ts = current_ts as u64;
    if now_ts <= last_ts {
        return;
    }

    let elapsed_seconds = now_ts - last_ts;
    // Linear annual decay: decay_rate% per year applied proportionally to elapsed time.
    // retained_pct = max(0, 100 - decay_rate * elapsed_years)
    // Use integer arithmetic: elapsed_years * 100 = elapsed_seconds * 100 / ONE_YEAR_IN_SECONDS
    let decay_amount = (decay_rate as u64) * elapsed_seconds / ONE_YEAR_IN_SECONDS;
    let retained_pct = 100_u64.saturating_sub(decay_amount);

    rep.total_score  = (rep.total_score  * retained_pct) / 100;
    rep.total_weight = (rep.total_weight * retained_pct) / 100;
    rep.last_updated_ts = current_ts;
}

/// Applies a 0-100 decay `factor` (percent) to `value` as `value * factor / 100`
/// using a u128 intermediate so large `value`s (already saturated to u64::MAX)
/// don't overflow before the division brings the result back under u64::MAX.
fn scale_by_factor_pct(value: u64, factor: u64) -> u64 {
    ((value as u128) * (factor as u128) / 100) as u64
}

fn get_decay_factor(decay_rate: u32, current_time: u64, recorded_at: u64) -> u64 {
    if decay_rate == 0 {
        return 100;
    }

    let age_in_seconds = current_time.saturating_sub(recorded_at);
    let decay_amount = (decay_rate as u64).saturating_mul(age_in_seconds) / ONE_YEAR_IN_SECONDS;

    100_u64.saturating_sub(decay_amount)
}

/// Calculate the reputation tier based on average rating score.
/// Score thresholds:
/// - 0-99: None
/// - 100-299: Bronze
/// - 300-499: Silver
/// - 500-699: Gold
/// - 700+: Platinum
fn calculate_tier(average_rating: u64) -> ReputationTier {
    if average_rating >= 700 {
        ReputationTier::Platinum
    } else if average_rating >= 500 {
        ReputationTier::Gold
    } else if average_rating >= 300 {
        ReputationTier::Silver
    } else if average_rating >= 100 {
        ReputationTier::Bronze
    } else {
        ReputationTier::None
    }
}

#[contract]
pub struct ReputationContract;

#[contractimpl]
impl ReputationContract {
    /// Submit a review for a user after completing a job.
    /// Rating must be between 1 and 5. Stake weight affects the review's influence.
    /// The escrow_contract_id is used to verify the job exists, is completed,
    /// and that reviewer/reviewee are the actual participants of the job.
    pub fn submit_review(
        env: Env,
        escrow_contract_id: Address,
        reviewer: Address,
        reviewee: Address,
        job_id: u64,
        rating: u32,
        comment: String,
        stake_weight: i128,
    ) -> Result<(), ReputationError> {
        reviewer.require_auth();
        require_not_paused(&env)?;

        if !(1..=5).contains(&rating) {
            return Err(ReputationError::InvalidRating);
        }
        if reviewer == reviewee {
            return Err(ReputationError::SelfReview);
        }

        // 1. Minimum Stake Check
        let min_stake = env
            .storage()
            .instance()
            .get(&DataKey::MinStake)
            .unwrap_or(MIN_REVIEW_STAKE_DEFAULT);
        if stake_weight < min_stake {
            return Err(ReputationError::BelowMinStake);
        }

        // 1b. Minimum Stake Weight Check — enforces a hard floor on the vote weight
        // regardless of how the economic min_stake is configured. A zero-stake review
        // would otherwise use weight=1 via the fallback path, letting stake-free
        // reviewers influence the leaderboard without any economic commitment.
        let min_stake_weight: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MinStakeWeight)
            .unwrap_or(MIN_STAKE_WEIGHT);
        if stake_weight < min_stake_weight as i128 {
            return Err(ReputationError::StakeTooLow);
        }

        // 2. Rate Limit Check
        let rate_limit = env
            .storage()
            .instance()
            .get(&DataKey::RateLimit)
            .unwrap_or(RATE_LIMIT_LEDGERS_DEFAULT);
        if rate_limit > 0 {
            let last_ledger_key = DataKey::LastReviewLedger(reviewer.clone());
            if let Some(last_ledger) = env
                .storage()
                .persistent()
                .get::<DataKey, u32>(&last_ledger_key)
            {
                let current_ledger = env.ledger().sequence();
                if current_ledger < last_ledger.saturating_add(rate_limit) {
                    return Err(ReputationError::RateLimitExceeded);
                }
            }
            let current_ledger = env.ledger().sequence();
            env.storage()
                .persistent()
                .set(&last_ledger_key, &current_ledger);
            // Extend TTL for rate limit data
            env.storage().persistent().extend_ttl(
                &last_ledger_key,
                MIN_TTL_THRESHOLD,
                MIN_TTL_EXTEND_TO,
            );

            // 2b. Per-reviewee rate limit (issue #1117): bounds how many reviews a
            // single reviewee can accumulate within one rate-limit window, so a
            // griefer cannot rapidly grow a target's Reviews vector even though
            // each individual review comes from a different reviewer/job.
            let window_key = DataKey::RevieweeReviewWindow(reviewee.clone());
            let (window_start, count): (u32, u32) = env
                .storage()
                .persistent()
                .get(&window_key)
                .unwrap_or((current_ledger, 0));

            let (window_start, count) = if current_ledger >= window_start.saturating_add(rate_limit)
            {
                (current_ledger, 0)
            } else {
                (window_start, count)
            };

            if count >= MAX_REVIEWS_PER_REVIEWEE_WINDOW {
                return Err(ReputationError::RateLimitExceeded);
            }

            env.storage()
                .persistent()
                .set(&window_key, &(window_start, count + 1));
            env.storage().persistent().extend_ttl(
                &window_key,
                MIN_TTL_THRESHOLD,
                MIN_TTL_EXTEND_TO,
            );
        }

        // Check if this reviewer already reviewed this user for this job
        let review_key = DataKey::ReviewExists(reviewer.clone(), reviewee.clone(), job_id);
        if env.storage().persistent().has(&review_key) {
            return Err(ReputationError::AlreadyReviewed);
        }

        // Cross-contract call: verify the job exists, is completed, and the
        // reviewer/reviewee are the actual client and freelancer of the job.
        let escrow_client = EscrowContractClient::new(&env, &escrow_contract_id);
        let job = match escrow_client.try_get_job(&job_id) {
            Ok(Ok(j)) => j,
            Ok(Err(_)) | Err(_) => return Err(ReputationError::JobNotFound),
        };

        if job.status != JobStatus::Completed {
            return Err(ReputationError::JobNotCompleted);
        }

        let valid_participants = (reviewer == job.client && reviewee == job.freelancer)
            || (reviewer == job.freelancer && reviewee == job.client);

        if !valid_participants {
            return Err(ReputationError::NotJobParticipant);
        }

        // 3. Accountability Stake Transfer
        // Transfer stake from reviewer to this contract
        let token_client = token::Client::new(&env, &job.token);
        token_client.transfer(&reviewer, &env.current_contract_address(), &stake_weight);

        // Track stake balance for withdrawal
        let balance_key = DataKey::StakeBalance(reviewer.clone());
        let mut balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);
        balance += stake_weight;
        env.storage().persistent().set(&balance_key, &balance);
        env.storage()
            .persistent()
            .extend_ttl(&balance_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        // Record which token this stake is denominated in so claim_stake can
        // transfer the correct asset back.
        let stake_token_key = DataKey::StakeToken(reviewer.clone());
        env.storage().persistent().set(&stake_token_key, &job.token);
        env.storage()
            .persistent()
            .extend_ttl(&stake_token_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        // Record (or refresh) the lockup start timestamp. We always write the
        // current timestamp so that incremental stakes cannot use an old anchor
        // to escape the lockup early. The full lockup window restarts from the
        // latest deposit, ensuring all accumulated stake is bound for
        // STAKE_LOCKUP_SECONDS from the most recent review (issue #exploit).
        let lockup_key = DataKey::StakeLockupTs(reviewer.clone());
        let current_ts = env.ledger().timestamp();
        env.storage().persistent().set(&lockup_key, &current_ts);
        env.storage()
            .persistent()
            .extend_ttl(&lockup_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        // Saturate stake_weight to u64::MAX instead of truncating to prevent
        // large i128 stakes from wrapping to arbitrary values (issue #982).
        // Then apply the per-review cap (MAX_STAKE_WEIGHT_PER_REVIEW) so that a
        // single self-dealt review with an arbitrarily large stake_weight cannot
        // dominate total_score and mint free governance power (issue #exploit).
        let raw_weight = if stake_weight > 0 {
            i128::min(stake_weight, u64::MAX as i128) as u64
        } else {
            1u64
        };
        let weight = raw_weight.min(MAX_STAKE_WEIGHT_PER_REVIEW);

        // Capture the old tier before mutating reputation so the tier_up event
        // can carry both the previous and new tier values.
        let old_avg_rating = Self::get_average_rating(env.clone(), reviewee.clone()).unwrap_or(0);
        let old_tier = calculate_tier(old_avg_rating);

        // Update user reputation

        let rep_key = DataKey::Reputation(reviewee.clone());
        let mut reputation: UserReputation =
            env.storage()
                .persistent()
                .get(&rep_key)
                .unwrap_or(UserReputation {
                    user: reviewee.clone(),
                    total_score: 0,
                    total_weight: 0,
                    review_count: 0,
                    last_updated_ts: env.ledger().timestamp() as u32,
                });

        apply_lazy_decay(&env, &mut reputation);

        reputation.total_score = reputation
            .total_score
            .saturating_add((rating as u64).saturating_mul(weight));
        reputation.total_weight = reputation.total_weight.saturating_add(weight);
        reputation.review_count += 1;
        reputation.last_updated_ts = env.ledger().timestamp() as u32;

        env.storage().persistent().set(&rep_key, &reputation);
        bump_reputation_ttl(&env, &reviewee);

        // Store review
        let review = Review {
            reviewer: reviewer.clone(),
            reviewee: reviewee.clone(),
            job_id,
            rating,
            comment: comment.clone(),
            stake_weight,
            timestamp: env.ledger().timestamp(),
        };

        let reviews_key = DataKey::Reviews(reviewee.clone());
        let mut reviews: Vec<Review> = env
            .storage()
            .persistent()
            .get(&reviews_key)
            .unwrap_or(Vec::new(&env));
        reviews.push_back(review);
        env.storage().persistent().set(&reviews_key, &reviews);
        bump_reviews_ttl(&env, &reviewee);

        // Mark as reviewed
        env.storage().persistent().set(&review_key, &true);
        bump_review_exists_ttl(&env, &reviewer, &reviewee, job_id);

        // Update leaderboard with the reviewee's new rating
        Self::update_leaderboard(&env, &reviewee);

        // Check for tier upgrade and award badge if necessary
        let new_avg_rating = Self::get_average_rating(env.clone(), reviewee.clone()).unwrap_or(0);
        let new_tier = calculate_tier(new_avg_rating);

        // Get existing badges to check if this tier badge already exists
        let badges_key = DataKey::Badges(reviewee.clone());
        let mut badges: Vec<AwardedBadge> = env
            .storage()
            .persistent()
            .get(&badges_key)
            .unwrap_or(Vec::new(&env));

        // Check if user already has this tier badge
        let has_tier_badge = badges.iter().any(|b| b.badge_type == new_tier);

        if !has_tier_badge && new_tier != ReputationTier::None {
            let badge = AwardedBadge {
                badge_type: new_tier,
                awarded_at: env.ledger().timestamp(),
            };
            badges.push_back(badge);
            env.storage().persistent().set(&badges_key, &badges);
            bump_badges_ttl(&env, &reviewee);

            // Emit badge awarded event
            env.events().publish(
                (symbol_short!("reput"), symbol_short!("badge")),
                (reviewee.clone(), new_tier),
            );

            // Emit tier upgrade event so indexers/backends have a dedicated
            // signal without needing to parse badge events.
            env.events().publish(
                (symbol_short!("reput"), symbol_short!("tier_up")),
                (reviewee.clone(), old_tier, new_tier),
            );
        }

        // Process referral bonuses for participating on a completed job
        Self::process_referral_bonus(&env, &reviewer);
        Self::process_referral_bonus(&env, &reviewee);

        // Emit event
        env.events().publish(
            (symbol_short!("reput"), symbol_short!("reviewed")),
            (reviewer, reviewee, job_id, rating, comment, stake_weight),
        );

        Ok(())
    }

    /// Register a referrer for a new user. Can only be called once per referree.
    pub fn register_referral(
        env: Env,
        referree: Address,
        referrer: Address,
    ) -> Result<(), ReputationError> {
        referree.require_auth();

        if referree == referrer {
            return Err(ReputationError::SelfReferral);
        }

        let ref_key = DataKey::Referrer(referree.clone());
        if env.storage().persistent().has(&ref_key) {
            return Err(ReputationError::AlreadyReferred);
        }

        // Guard against circular referral chains (max depth check)
        let mut current = referrer.clone();
        let mut depth = 0;
        while let Some(ancestor) = env
            .storage()
            .persistent()
            .get::<DataKey, Address>(&DataKey::Referrer(current.clone()))
        {
            if ancestor == referree {
                return Err(ReputationError::CircularReferral);
            }
            current = ancestor;
            depth += 1;
            if depth > 10 {
                break;
            } // Bounded safety limit
        }

        env.storage().persistent().set(&ref_key, &referrer);
        env.storage()
            .persistent()
            .extend_ttl(&ref_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        // Initialize or update referrer stats
        let stats_key = DataKey::ReferralStats(referrer.clone());
        let mut stats = env
            .storage()
            .persistent()
            .get::<DataKey, ReferralStats>(&stats_key)
            .unwrap_or(ReferralStats {
                total_referrals: 0,
                earned_bonus: 0,
            });
        stats.total_referrals += 1;

        env.storage().persistent().set(&stats_key, &stats);
        env.storage()
            .persistent()
            .extend_ttl(&stats_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("referred")),
            (referree, referrer),
        );

        Ok(())
    }

    /// Alias for `register_referral` using the naming from the public API proposal.
    pub fn register_with_referrer(
        env: Env,
        user: Address,
        referrer: Address,
    ) -> Result<(), ReputationError> {
        Self::register_referral(env, user, referrer)
    }

    /// Retrieve the stats for a referrer
    pub fn get_referral_stats(env: Env, referrer: Address) -> ReferralStats {
        let stats_key = DataKey::ReferralStats(referrer.clone());
        let stats: Option<ReferralStats> = env.storage().persistent().get(&stats_key);
        match stats {
            Some(s) => {
                env.storage().persistent().extend_ttl(
                    &stats_key,
                    MIN_TTL_THRESHOLD,
                    MIN_TTL_EXTEND_TO,
                );
                s
            }
            None => ReferralStats {
                total_referrals: 0,
                earned_bonus: 0,
            },
        }
    }

    /// Internal: Credit the bonus to the user's referrer upon their first completed job.
    fn process_referral_bonus(env: &Env, user: &Address) {
        let bonus_paid_key = DataKey::BonusPaid(user.clone());
        if env.storage().persistent().has(&bonus_paid_key) {
            return; // Bonus already paid out for this referree
        }

        let ref_key = DataKey::Referrer(user.clone());
        if let Some(referrer) = env.storage().persistent().get::<DataKey, Address>(&ref_key) {
            // Mark as paid
            env.storage().persistent().set(&bonus_paid_key, &true);
            env.storage().persistent().extend_ttl(
                &bonus_paid_key,
                MIN_TTL_THRESHOLD,
                MIN_TTL_EXTEND_TO,
            );

            // Credit reputation as one virtual review at REFERRAL_BONUS_REPUTATION_WEIGHT
            // with rating `bonus_rating` (avoids min_stake scaling, which inflated totals).
            // Stored as a record for decay calculation.
            let bonus_rating = env
                .storage()
                .instance()
                .get::<DataKey, u64>(&DataKey::ReferralBonus)
                .unwrap_or(DEFAULT_REFERRAL_BONUS);
            let weight = REFERRAL_BONUS_REPUTATION_WEIGHT;
            let earned_score = bonus_rating * weight;

            let bonuses_key = DataKey::ReferralBonusList(referrer.clone());
            let mut bonuses: Vec<ReferralBonusRecord> = env
                .storage()
                .persistent()
                .get(&bonuses_key)
                .unwrap_or(Vec::new(env));

            bonuses.push_back(ReferralBonusRecord {
                amount: earned_score,
                weight,
                timestamp: env.ledger().timestamp(),
            });

            env.storage().persistent().set(&bonuses_key, &bonuses);
            env.storage().persistent().extend_ttl(
                &bonuses_key,
                MIN_TTL_THRESHOLD,
                MIN_TTL_EXTEND_TO,
            );

            // Update legacy accumulator (optional but good for redundant check if needed)
            let rep_key = DataKey::Reputation(referrer.clone());
            let mut reputation: UserReputation = env
                .storage()
                .persistent()
                .get(&rep_key)
                .unwrap_or(UserReputation {
                    user: referrer.clone(),
                    total_score: 0,
                    total_weight: 0,
                    review_count: 0,
                    last_updated_ts: env.ledger().timestamp() as u32,
                });

            apply_lazy_decay(env, &mut reputation);

            reputation.total_score += earned_score;
            reputation.total_weight += weight;
            reputation.last_updated_ts = env.ledger().timestamp() as u32;

            env.storage().persistent().set(&rep_key, &reputation);
            bump_reputation_ttl(env, &referrer);

            // Update Referral Stats
            let stats_key = DataKey::ReferralStats(referrer.clone());
            let mut stats = env
                .storage()
                .persistent()
                .get::<DataKey, ReferralStats>(&stats_key)
                .unwrap_or(ReferralStats {
                    total_referrals: 1,
                    earned_bonus: 0,
                });
            stats.earned_bonus += earned_score;

            env.storage().persistent().set(&stats_key, &stats);
            env.storage()
                .persistent()
                .extend_ttl(&stats_key, MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

            env.events().publish(
                (symbol_short!("reput"), symbol_short!("ref_rwrd")),
                (referrer.clone(), earned_score, user.clone()),
            );

            env.events().publish(
                (symbol_short!("reput"), Symbol::new(env, "referral_reward")),
                (referrer, earned_score),
            );
        }
    }

    /// Set configuration for the referral bonus (multi-sig only)
    pub fn set_referral_bonus(
        env: Env,
        signer: Address,
        bonus: u64,
    ) -> Result<(), ReputationError> {
        signer.require_auth();
        if !is_signer(&env, &signer) {
            return Err(ReputationError::NotAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::ReferralBonus, &bonus);
        bump_instance_ttl(&env);
        Ok(())
    }

    /// Record a referral bonus for `user` with an explicit `timestamp`.
    ///
    /// Restricted to registered multi-sig signers — used for migrations and
    /// manual corrections that need to backfill a bonus with its original date.
    ///
    /// Security (issue #781): the `timestamp` is validated to be at or before the
    /// current ledger time. A future-dated bonus would make `get_decay_factor`
    /// compute `elapsed_seconds = 0` forever, permanently exempting the bonus
    /// from time decay and inflating the user's score regardless of how much
    /// time actually passes. Past and current timestamps are accepted so the
    /// bonus decays from its true origin date.
    pub fn add_referral_bonus(
        env: Env,
        signer: Address,
        user: Address,
        amount: u64,
        weight: u64,
        timestamp: u64,
    ) -> Result<(), ReputationError> {
        signer.require_auth();
        if !is_signer(&env, &signer) {
            return Err(ReputationError::NotAdmin);
        }
        require_not_paused(&env)?;

        // Reject future timestamps that would bypass decay (see doc comment).
        if timestamp > env.ledger().timestamp() {
            return Err(ReputationError::InvalidTimestamp);
        }

        let bonuses_key = DataKey::ReferralBonusList(user.clone());
        let mut bonuses: Vec<ReferralBonusRecord> = env
            .storage()
            .persistent()
            .get(&bonuses_key)
            .unwrap_or(Vec::new(&env));
        bonuses.push_back(ReferralBonusRecord {
            amount,
            weight,
            timestamp,
        });
        env.storage().persistent().set(&bonuses_key, &bonuses);
        env.storage().persistent().extend_ttl(
            &bonuses_key,
            MIN_TTL_THRESHOLD,
            MIN_TTL_EXTEND_TO,
        );

        // Mirror `process_referral_bonus`: keep the legacy reputation accumulator
        // present so `get_reputation` resolves the user. The decayed totals are
        // always recomputed from the bonus list, so this stays consistent.
        let rep_key = DataKey::Reputation(user.clone());
        let mut reputation: UserReputation = env
            .storage()
            .persistent()
            .get(&rep_key)
            .unwrap_or(UserReputation {
                user: user.clone(),
                total_score: 0,
                total_weight: 0,
                review_count: 0,
                last_updated_ts: env.ledger().timestamp() as u32,
            });
        apply_lazy_decay(&env, &mut reputation);
        reputation.total_score = reputation.total_score.saturating_add(amount);
        reputation.total_weight = reputation.total_weight.saturating_add(weight);
        reputation.last_updated_ts = env.ledger().timestamp() as u32;
        env.storage().persistent().set(&rep_key, &reputation);
        bump_reputation_ttl(&env, &user);

        // Keep the leaderboard consistent with the user's new decayed totals.
        Self::update_leaderboard(&env, &user);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("ref_add")),
            (user, amount, timestamp),
        );

        Ok(())
    }

    /// Update the annual reputation `decay_rate` (percent per year).
    ///
    /// Restricted to registered multi-sig signers.
    ///
    /// Security (issue #783): the rate is bounded by the configurable maximum
    /// (`effective_max_decay_rate`, default `MAX_DECAY_RATE`). Without an upper
    /// bound, a rate such as 99 would erase ~99% of every score within a year,
    /// emptying the leaderboard and rendering reputation meaningless. Values
    /// above the maximum are rejected with `DecayRateTooHigh`.
    pub fn update_decay_rate(
        env: Env,
        signer: Address,
        decay_rate: u32,
    ) -> Result<(), ReputationError> {
        signer.require_auth();
        if !is_signer(&env, &signer) {
            return Err(ReputationError::NotAdmin);
        }
        if decay_rate > effective_max_decay_rate(&env) {
            return Err(ReputationError::DecayRateTooHigh);
        }
        env.storage().instance().set(&DataKey::DecayRate, &decay_rate);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("decay_set")),
            decay_rate,
        );

        Ok(())
    }

    /// Set the configurable maximum allowed `decay_rate` (super-admin only).
    ///
    /// Restricted to registered multi-sig signers and itself capped at
    /// `MAX_DECAY_RATE_HARD_CEILING` (50%) so that even a compromised admin
    /// cannot raise the ceiling high enough to destroy all reputation (issue
    /// #783). Requests above the hard ceiling are rejected with
    /// `DecayRateTooHigh`.
    pub fn set_max_decay_rate(
        env: Env,
        signer: Address,
        rate: u32,
    ) -> Result<(), ReputationError> {
        signer.require_auth();
        if !is_signer(&env, &signer) {
            return Err(ReputationError::NotAdmin);
        }
        if rate > MAX_DECAY_RATE_HARD_CEILING {
            return Err(ReputationError::DecayRateTooHigh);
        }
        env.storage().instance().set(&DataKey::MaxDecayRate, &rate);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("max_decay")),
            rate,
        );

        Ok(())
    }

    /// Get the reputation data for a user, applying time decay to totals.
    pub fn get_reputation(env: Env, user: Address) -> Result<UserReputation, ReputationError> {
        bump_instance_ttl(&env);
        let rep_key = DataKey::Reputation(user.clone());
        if !env.storage().persistent().has(&rep_key) {
            return Err(ReputationError::UserNotFound);
        }

        let (total_score, total_weight, review_count) =
            Self::get_decayed_totals(&env, user.clone());

        bump_reputation_ttl(&env, &user);
        Ok(UserReputation {
            user,
            total_score,
            total_weight,
            review_count,
            last_updated_ts: env.ledger().timestamp() as u32,
        })
    }

    /// Governance weight snapshot helper (issue #899).
    ///
    /// Returns `(score, last_change_ts)` where:
    /// - `score` is the user's current decayed, stake-weighted reputation total
    ///   (`total_score` from [`get_decayed_totals`]) — used as raw voting weight
    ///   by the reputation-weighted governance path in the escrow contract.
    /// - `last_change_ts` is the ledger timestamp of the user's most recent
    ///   *score-changing* event (review, slash, dispute outcome, referral bonus,
    ///   appeal resolution). It is read from the stored [`UserReputation`], whose
    ///   `last_updated_ts` is bumped by every such mutation and is **not**
    ///   moved by pure reads.
    ///
    /// # Snapshot safety
    ///
    /// Governance compares `last_change_ts` against a proposal's snapshot
    /// timestamp and only accepts a vote when `last_change_ts <= snapshot_ts`.
    /// This makes reputation acquired *after* a proposal opens ineligible to vote
    /// on it, defeating the classic "pump reputation to swing an in-flight vote"
    /// attack. Because the returned score is the decayed value read at call time
    /// and decay is monotonically non-increasing, the weight can only be *lower*
    /// than it was at snapshot time — never inflated — so the check is safe.
    ///
    /// Unknown users return `(0, 0)`: zero weight, and a `last_change_ts` of `0`
    /// that is `<=` any snapshot (they simply have no weight to cast).
    pub fn get_gov_weight(env: Env, user: Address) -> (u64, u64) {
        bump_instance_ttl(&env);
        let stored: Option<UserReputation> = env
            .storage()
            .persistent()
            .get(&DataKey::Reputation(user.clone()));
        let last_change_ts = match stored {
            Some(rep) => rep.last_updated_ts as u64,
            // No reputation record: no weight, and a snapshot-safe timestamp of 0.
            None => return (0, 0),
        };
        let (total_score, _total_weight, _review_count) = Self::get_decayed_totals(&env, user);
        // Cap the returned score at MAX_GOV_WEIGHT so that even if per-review
        // capping is somehow bypassed in a future code path, a single user's
        // raw total_score cannot translate into unbounded governance voting power
        // (issue #exploit, defense-in-depth).
        let capped_score = total_score.min(MAX_GOV_WEIGHT);
        (capped_score, last_change_ts)
    }

    /// Get reputation together with the registered referrer (if any).
    pub fn get_reputation_with_referrer(
        env: Env,
        user: Address,
    ) -> Result<UserReputationWithReferrer, ReputationError> {
        let base = Self::get_reputation(env.clone(), user.clone())?;
        let referrer: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Referrer(user.clone()));
        if referrer.is_some() {
            env.storage().persistent().extend_ttl(
                &DataKey::Referrer(user.clone()),
                MIN_TTL_THRESHOLD,
                MIN_TTL_EXTEND_TO,
            );
        }

        Ok(UserReputationWithReferrer {
            user,
            referrer,
            total_score: base.total_score,
            total_weight: base.total_weight,
            review_count: base.review_count,
        })
    }

    /// Initialize the reputation contract with signers.
    pub fn initialize(
        env: Env,
        signers: Vec<Address>,
        threshold: u32,
        decay_rate: u32,
    ) -> Result<(), ReputationError> {
        if env.storage().instance().has(&DataKey::MultiSigSigners) {
            return Err(ReputationError::Unauthorized); // already initialized
        }
        if decay_rate > 100 {
            return Err(ReputationError::InvalidDecayRate);
        }
        if threshold == 0 || threshold > signers.len() {
            return Err(ReputationError::NotAdmin); // Or a specific error if available
        }

        env.storage()
            .instance()
            .set(&DataKey::MultiSigSigners, &signers);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigThreshold, &threshold);

        env.storage()
            .instance()
            .set(&DataKey::DecayRate, &decay_rate);
        env.storage()
            .instance()
            .set(&DataKey::MinStake, &MIN_REVIEW_STAKE_DEFAULT);
        env.storage()
            .instance()
            .set(&DataKey::RateLimit, &RATE_LIMIT_LEDGERS_DEFAULT);
        env.storage().instance().set(&DataKey::Paused, &false);
        bump_instance_ttl(&env);
        Ok(())
    }

    /// Configure the dispute contract allowed to slash reputation.
    /// This is a privileged action and must be performed by a registered signer.
    pub fn set_dispute_contract(
        env: Env,
        signer: Address,
        dispute_contract: Address,
    ) -> Result<(), ReputationError> {
        signer.require_auth();
        if !is_signer(&env, &signer) {
            return Err(ReputationError::NotAdmin);
        }

        env.storage()
            .instance()
            .set(&DataKey::DisputeContract, &dispute_contract);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "dispute_set")),
            (signer, dispute_contract),
        );

        Ok(())
    }

    /// Slash a user's reputation score. Callable only by the configured dispute contract.
    pub fn slash_reputation(
        env: Env,
        user: Address,
        job_id: u64,
        amount: u64,
        reason: String,
    ) -> Result<(), ReputationError> {
        require_not_paused(&env)?;

        let dispute_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::DisputeContract)
            .ok_or(ReputationError::NotInitialized)?;
        dispute_contract.require_auth();

        let rep_key = DataKey::Reputation(user.clone());
        let mut reputation: UserReputation =
            env.storage()
                .persistent()
                .get(&rep_key)
                .unwrap_or(UserReputation {
                    user: user.clone(),
                    total_score: 0,
                    total_weight: 0,
                    review_count: 0,
                    last_updated_ts: env.ledger().timestamp() as u32,
                });

        apply_lazy_decay(&env, &mut reputation);

        reputation.total_score = reputation.total_score.saturating_sub(amount);
        reputation.last_updated_ts = env.ledger().timestamp() as u32;
        env.storage().persistent().set(&rep_key, &reputation);
        bump_reputation_ttl(&env, &user);

        env.events().publish(
            (
                symbol_short!("reput"),
                Symbol::new(&env, "reputation_slashed"),
            ),
            (user, job_id, amount, reason),
        );

        Ok(())
    }

    /// Apply dispute outcome to a user's reputation score. Callable only by the configured dispute contract.
    /// Updates reputation based on outcome:
    /// - Won: +50 points
    /// - Lost: -100 points
    /// - MaliciousFiling: -250 points
    pub fn apply_dispute_outcome(
        env: Env,
        user: Address,
        outcome: DisputeOutcome,
    ) -> Result<(), ReputationError> {
        require_not_paused(&env)?;

        let dispute_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::DisputeContract)
            .ok_or(ReputationError::NotInitialized)?;
        dispute_contract.require_auth();

        let score_change: i64 = match outcome {
            DisputeOutcome::Won => 50,
            DisputeOutcome::Lost => -100,
            DisputeOutcome::MaliciousFiling => -250,
        };

        let rep_key = DataKey::Reputation(user.clone());
        let mut reputation: UserReputation = env
            .storage()
            .persistent()
            .get(&rep_key)
            .unwrap_or(UserReputation {
                user: user.clone(),
                total_score: 0,
                total_weight: 0,
                review_count: 0,
                last_updated_ts: env.ledger().timestamp() as u32,
            });

        apply_lazy_decay(&env, &mut reputation);

        if score_change > 0 {
            reputation.total_score = reputation.total_score.saturating_add(score_change as u64);
        } else {
            reputation.total_score = reputation.total_score.saturating_sub((-score_change) as u64);
        }

        reputation.last_updated_ts = env.ledger().timestamp() as u32;
        env.storage().persistent().set(&rep_key, &reputation);
        bump_reputation_ttl(&env, &user);

        // Update leaderboard with the user's new rating
        Self::update_leaderboard(&env, &user);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "dispute_outcome")),
            (user, outcome, score_change),
        );

        Ok(())
    }

    /// Get the current minimum stake requirement.
    pub fn get_min_stake(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::MinStake)
            .unwrap_or(MIN_REVIEW_STAKE_DEFAULT)
    }

    /// Get the current rate limit in ledgers.
    pub fn get_rate_limit(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::RateLimit)
            .unwrap_or(RATE_LIMIT_LEDGERS_DEFAULT)
    }

    /// Get the current minimum stake weight threshold.
    pub fn get_min_stake_weight(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinStakeWeight)
            .unwrap_or(MIN_STAKE_WEIGHT)
    }

    /// Set the minimum stake weight threshold (admin/signer only).
    /// Reviews submitted with stake_weight below this value are rejected with StakeTooLow.
    pub fn set_min_stake_weight(
        env: Env,
        admin: Address,
        weight: u64,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::MinStakeWeight, &weight);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "min_wt_set")),
            (admin, weight),
        );

        Ok(())
    }

    /// Ban a user, excluding them from all leaderboard queries (admin/signer only).
    pub fn ban_user(env: Env, admin: Address, user: Address) -> Result<(), ReputationError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }
        env.storage()
            .instance()
            .set(&DataKey::BannedUser(user.clone()), &true);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "banned")),
            (user, admin),
        );

        Ok(())
    }

    /// Unban a user, restoring their visibility on the leaderboard (admin/signer only).
    pub fn unban_user(env: Env, admin: Address, user: Address) -> Result<(), ReputationError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }
        env.storage()
            .instance()
            .remove(&DataKey::BannedUser(user.clone()));
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "unbanned")),
            (user, admin),
        );

        Ok(())
    }

    pub fn propose_admin_action(
        env: Env,
        proposer: Address,
        action: AdminAction,
    ) -> Result<u64, ReputationError> {
        proposer.require_auth();
        if !is_signer(&env, &proposer) {
            return Err(ReputationError::NotAdmin);
        }

        let mut count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigProposalCount)
            .unwrap_or(0);
        count += 1;

        let mut approvals = Vec::new(&env);
        approvals.push_back(proposer.clone());

        let proposal = MultiSigProposal {
            id: count,
            action: action.clone(),
            proposer: proposer.clone(),
            approvals,
            executed: false,
            created_at: env.ledger().timestamp(),
        };

        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposal(count), &proposal);
        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposalCount, &count);

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("proposed")),
            (count, proposer, action),
        );

        let threshold: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigThreshold)
            .unwrap_or(1);
        if threshold == 1 {
            Self::execute_proposal(&env, count)?;
        }

        Ok(count)
    }

    pub fn approve_admin_action(
        env: Env,
        approver: Address,
        proposal_id: u64,
    ) -> Result<(), ReputationError> {
        approver.require_auth();
        if !is_signer(&env, &approver) {
            return Err(ReputationError::NotAdmin);
        }

        let mut proposal: MultiSigProposal = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigProposal(proposal_id))
            .ok_or(ReputationError::NotAdmin)?;

        if proposal.executed {
            return Err(ReputationError::Unauthorized);
        }

        if proposal.approvals.iter().any(|a| a == approver) {
            return Err(ReputationError::Unauthorized);
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
            Self::execute_proposal(&env, proposal_id)?;
        }

        Ok(())
    }

    fn execute_proposal(env: &Env, proposal_id: u64) -> Result<(), ReputationError> {
        let mut proposal: MultiSigProposal = env
            .storage()
            .instance()
            .get(&DataKey::MultiSigProposal(proposal_id))
            .ok_or(ReputationError::NotAdmin)?;

        if proposal.executed {
            return Err(ReputationError::Unauthorized);
        }

        match proposal.action.clone() {
            AdminAction::Pause => {
                env.storage().instance().set(&DataKey::Paused, &true);
                env.events().publish(
                    (symbol_short!("reput"), symbol_short!("paused")),
                    (env.current_contract_address(), env.ledger().timestamp()),
                );
            }
            AdminAction::Unpause => {
                env.storage().instance().set(&DataKey::Paused, &false);
                env.events().publish(
                    (symbol_short!("reput"), symbol_short!("unpaused")),
                    (env.current_contract_address(), env.ledger().timestamp()),
                );
            }
            AdminAction::SetMinStake(amount) => {
                env.storage().instance().set(&DataKey::MinStake, &amount);
            }
            AdminAction::SetRateLimit(ledgers) => {
                env.storage().instance().set(&DataKey::RateLimit, &ledgers);
            }
            AdminAction::SetToken(token) => {
                env.storage().instance().set(&DataKey::Token, &token);
            }
            AdminAction::SetReferralBonus(bonus) => {
                env.storage()
                    .instance()
                    .set(&DataKey::ReferralBonus, &bonus);
            }
            AdminAction::AddSigner(signer) => {
                let mut signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                if !signers.iter().any(|s| s == signer) {
                    signers.push_back(signer);
                    env.storage()
                        .instance()
                        .set(&DataKey::MultiSigSigners, &signers);
                }
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

                if let Some(idx) = signers.iter().position(|s| s == signer) {
                    if signers.len() <= threshold {
                        return Err(ReputationError::NotAdmin);
                    }
                    signers.remove(idx as u32);
                    env.storage()
                        .instance()
                        .set(&DataKey::MultiSigSigners, &signers);
                } else {
                    return Err(ReputationError::NotAdmin);
                }
            }
            AdminAction::ChangeThreshold(new_threshold) => {
                let signers: Vec<Address> = env
                    .storage()
                    .instance()
                    .get(&DataKey::MultiSigSigners)
                    .unwrap();
                if new_threshold == 0 || new_threshold > signers.len() {
                    return Err(ReputationError::NotAdmin);
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
                    return Err(ReputationError::NotAdmin);
                }
            }
            AdminAction::SetDecayRate(rate) => {
                // Enforce the same upper bound as `update_decay_rate` so the
                // multi-sig governance path cannot set a destructive decay rate
                // (issue #783).
                if rate > effective_max_decay_rate(env) {
                    return Err(ReputationError::DecayRateTooHigh);
                }
                env.storage().instance().set(&DataKey::DecayRate, &rate);
            }
            AdminAction::SlashStake(loser, job_id, amount) => {
                let rep_key = DataKey::Reputation(loser.clone());
                let mut reputation: UserReputation = env
                    .storage()
                    .persistent()
                    .get(&rep_key)
                    .unwrap_or(UserReputation {
                        user: loser.clone(),
                        total_score: 0,
                        total_weight: 0,
                        review_count: 0,
                        last_updated_ts: env.ledger().timestamp() as u32,
                    });

                apply_lazy_decay(&env, &mut reputation);

                reputation.total_score = reputation.total_score.saturating_sub(amount);
                reputation.last_updated_ts = env.ledger().timestamp() as u32;
                env.storage().persistent().set(&rep_key, &reputation);
                bump_reputation_ttl(env, &loser);

                env.events().publish(
                    (symbol_short!("reput"), symbol_short!("slashed")),
                    (loser, job_id, amount),
                );
            }
            AdminAction::SetStakeTiers(tiers) => {
                if tiers.len() > MAX_STAKE_TIERS {
                    return Err(ReputationError::TooManyStakeTiers);
                }
                env.storage().instance().set(&DataKey::StakeTiers, &tiers);
            }
        }

        proposal.executed = true;
        env.storage()
            .instance()
            .set(&DataKey::MultiSigProposal(proposal_id), &proposal);

        env.events().publish(
            (symbol_short!("msig"), symbol_short!("executed")),
            (proposal_id, proposal.action),
        );

        Ok(())
    }

    /// Calculate effective weight of a review, applying time decay.
    /// Formula: effective_weight = stake_weight * max(0, 100 - decay_rate * age_in_seconds / ONE_YEAR) / 100
    pub fn get_effective_weight(env: Env, review: Review, current_time: u64) -> i128 {
        let decay_rate: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DecayRate)
            .unwrap_or(0);

        let initial_weight = if review.stake_weight > 0 {
            review.stake_weight
        } else {
            1_i128
        };

        if decay_rate == 0 {
            return initial_weight;
        }

        let decay_factor = get_decay_factor(decay_rate, current_time, review.timestamp);

        if decay_factor == 0 {
            return 0;
        }

        (initial_weight.saturating_mul(decay_factor as i128)) / 100
    }

    fn get_decayed_totals(env: &Env, user: Address) -> (u64, u64, u32) {
        env.storage()
            .instance()
            .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);

        let decay_rate: u32 = env
            .storage()
            .instance()
            .get(&DataKey::DecayRate)
            .unwrap_or(0);
        let current_ts = env.ledger().timestamp();

        let reviews_key = DataKey::Reviews(user.clone());
        let reviews: Vec<Review> = env
            .storage()
            .persistent()
            .get(&reviews_key)
            .unwrap_or(Vec::new(env));

        // Extend TTL on read so that a frequently-read entry never silently expires.
        if !reviews.is_empty() {
            bump_reviews_ttl(env, &user);
        }

        let review_count = reviews.len() as u32;
        let mut total_score = 0u64;
        let mut total_weight = 0u64;

        // Score only the most recent MAX_REVIEWS_COUNTED reviews so this
        // function's cost stays bounded regardless of how many reviews `user`
        // has ever received (issue #1117). Reviews are appended in order, so
        // the tail of the vector is the most recent.
        let start = review_count.saturating_sub(MAX_REVIEWS_COUNTED);
        for idx in start..review_count {
            if let Some(review) = reviews.get(idx) {
                let factor = get_decay_factor(decay_rate, current_ts, review.timestamp);
                // Saturate stake_weight to u64::MAX to prevent truncation (issue #982).
                // Then cap at MAX_STAKE_WEIGHT_PER_REVIEW so that historical reviews
                // submitted before the per-review cap was enforced in submit_review
                // cannot still contribute unbounded weight on the read path (issue #exploit).
                let clamped_weight = i128::min(review.stake_weight, u64::MAX as i128) as u64;
                let capped_weight = clamped_weight.min(MAX_STAKE_WEIGHT_PER_REVIEW);
                let decayed_weight = scale_by_factor_pct(capped_weight, factor);
                total_score = total_score
                    .saturating_add((review.rating as u64).saturating_mul(decayed_weight));
                total_weight = total_weight.saturating_add(decayed_weight);
            }
        }

        // Include referral bonuses (stored as ReferralBonusRecord with individual timestamps),
        // similarly bounded to the most recent MAX_REVIEWS_COUNTED entries.
        let bonuses_key = DataKey::ReferralBonusList(user.clone());
        if let Some(bonuses) = env
            .storage()
            .persistent()
            .get::<DataKey, Vec<ReferralBonusRecord>>(&bonuses_key)
        {
            let bonus_count = bonuses.len() as u32;
            let start = bonus_count.saturating_sub(MAX_REVIEWS_COUNTED);
            for idx in start..bonus_count {
                if let Some(bonus) = bonuses.get(idx) {
                    let factor = get_decay_factor(decay_rate, current_ts, bonus.timestamp);
                    // bonus.amount = bonus_rating * bonus.weight; apply same decay factor.
                    total_score =
                        total_score.saturating_add(scale_by_factor_pct(bonus.amount, factor));
                    total_weight =
                        total_weight.saturating_add(scale_by_factor_pct(bonus.weight, factor));
                }
            }
        }

        (total_score, total_weight, review_count)
    }

    pub fn endorse(
        env: Env,
        endorser: Address,
        target: Address,
        skill: String,
    ) -> Result<(), ReputationError> {
        endorser.require_auth();
        require_not_paused(&env)?;

        if endorser == target {
            return Err(ReputationError::SelfEndorsement);
        }

        let key = DataKey::Endorsement(target.clone(), skill.clone(), endorser.clone());
        if env.storage().persistent().has(&key) {
            return Err(ReputationError::AlreadyEndorsed);
        }

        env.storage().persistent().set(&key, &true);

        let list_key = DataKey::SkillEndorsers(target.clone(), skill.clone());
        let mut endorsers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&list_key)
            .unwrap_or(Vec::new(&env));
        endorsers.push_back(endorser.clone());
        env.storage().persistent().set(&list_key, &endorsers);
        bump_endorsement_ttl(&env, &target, &skill, &endorser);

        Ok(())
    }

    /// Compute a user's skill score as the sum, over everyone who has
    /// endorsed them for `skill`, of each endorser's weight — their
    /// `get_average_rating() / 100`, or `1` if they have no rating yet.
    ///
    /// Security (issue #987): `endorse` rejects `endorser == target`, so a
    /// user cannot inflate this score by endorsing themselves. The number of
    /// endorsers summed is additionally capped at `MAX_ENDORSERS_COUNTED` so
    /// that neither this function's cost nor the score it returns can grow
    /// without bound as a skill accumulates endorsers.
    pub fn get_skill_score(env: Env, user: Address, skill: String) -> u32 {
        let list_key = DataKey::SkillEndorsers(user.clone(), skill.clone());
        let endorsers: Vec<Address> = env
            .storage()
            .persistent()
            .get(&list_key)
            .unwrap_or(Vec::new(&env));

        let mut score = 0;
        for endorser in endorsers.iter().take(MAX_ENDORSERS_COUNTED as usize) {
            let avg_rating = Self::get_average_rating(env.clone(), endorser.clone()).unwrap_or(0);
            let weight = if avg_rating > 0 { avg_rating / 100 } else { 1 };
            score += weight as u32;
        }

        score
    }

    pub fn set_stake_tiers(
        env: Env,
        admin: Address,
        tiers: Vec<StakeTier>,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }
        if tiers.len() > MAX_STAKE_TIERS {
            return Err(ReputationError::TooManyStakeTiers);
        }
        env.storage().instance().set(&DataKey::StakeTiers, &tiers);
        bump_instance_ttl(&env);

        env.events().publish(
            (symbol_short!("reput"), Symbol::new(&env, "tiers_set")),
            (admin, tiers.clone()),
        );

        Ok(())
    }

    pub fn get_stake_multiplier(env: Env, user: Address) -> u32 {
        let balance_key = DataKey::StakeBalance(user.clone());
        let balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);

        let tiers: Vec<StakeTier> = env
            .storage()
            .instance()
            .get(&DataKey::StakeTiers)
            .unwrap_or(Vec::new(&env));
        let mut multiplier = 100; // Default 1x

        for tier in tiers.iter() {
            if balance >= tier.threshold {
                multiplier = tier.multiplier;
            }
        }
        multiplier
    }

    /// Returns the raw staked token balance currently held for a reviewer.
    /// Returns 0 if the reviewer has no active stake.
    pub fn get_stake_balance(env: Env, user: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::StakeBalance(user))
            .unwrap_or(0)
    }

    pub fn get_average_rating(env: Env, user: Address) -> Result<u64, ReputationError> {
        let multiplier = Self::get_stake_multiplier(env.clone(), user.clone());

        let (total_score, total_weight, _) = Self::get_decayed_totals(&env, user);

        if total_weight == 0 {
            return Ok(0); // If completely decayed, acts as no rep
        }

        let base_score = ((total_score as u128) * 100 / (total_weight as u128)) as u64;
        let weighted = scale_by_factor_pct(base_score, multiplier as u64);
        Ok(weighted.min(10_000))
    }

    /// Get the total number of reviews for a user.
    pub fn get_review_count(env: Env, user: Address) -> u32 {
        let rep_key = DataKey::Reputation(user);
        let reputation: Option<UserReputation> = env.storage().persistent().get(&rep_key);
        match reputation {
            Some(rep) => {
                bump_reputation_ttl(&env, &rep.user);
                rep.review_count
            }
            None => 0,
        }
    }

    /// Get all reviews for a user.
    pub fn get_reviews(env: Env, user: Address) -> Vec<Review> {
        let reviews_key = DataKey::Reviews(user);
        let reviews: Option<Vec<Review>> = env.storage().persistent().get(&reviews_key);
        match reviews {
            Some(list) => {
                env.storage().persistent().extend_ttl(
                    &reviews_key,
                    MIN_TTL_THRESHOLD,
                    MIN_TTL_EXTEND_TO,
                );
                list
            }
            None => Vec::new(&env),
        }
    }

    /// Get the **rating tier** for a user — their standing on the *quality* ladder,
    /// thresholded on `average_rating` (Bronze ≥ 100, Silver ≥ 300, Gold ≥ 500,
    /// Platinum ≥ 700 on the 0–10,000 scale).
    ///
    /// Because `average_rating` is an average, this is independent of how many
    /// reviews a user has. It is **not** the same measure as
    /// [`Self::get_score_badge`], which ranks accumulated volume — the two use
    /// different inputs and thresholds and often disagree. See the module docs
    /// (issue #1172).
    pub fn get_tier(env: Env, user: Address) -> ReputationTier {
        match Self::get_average_rating(env, user) {
            Ok(avg_rating) => calculate_tier(avg_rating),
            Err(_) => ReputationTier::None,
        }
    }

    /// Get every **rating tier** the user has ever reached, as permanent
    /// [`AwardedBadge`] records (`badge_type` is a [`ReputationTier`]).
    ///
    /// Unrelated to [`Self::get_score_badge`], and not a live view: these records
    /// are written by `submit_review` when a tier is first crossed and are never
    /// removed, so a user can hold a `Gold` awarded badge while their current tier
    /// and score badge have decayed below it.
    pub fn get_badges(env: Env, user: Address) -> Vec<AwardedBadge> {
        let badges_key = DataKey::Badges(user);
        let badges: Option<Vec<AwardedBadge>> = env.storage().persistent().get(&badges_key);
        match badges {
            Some(list) => {
                env.storage().persistent().extend_ttl(
                    &badges_key,
                    MIN_TTL_THRESHOLD,
                    MIN_TTL_EXTEND_TO,
                );
                list
            }
            None => Vec::new(&env),
        }
    }

    /// Get the current **score badge** for a user — their standing on the *volume*
    /// ladder, thresholded on the raw decayed `total_score`:
    /// - [`ScoreBadge::Rising`]: score ≥ 100
    /// - [`ScoreBadge::Established`]: score ≥ 500
    /// - [`ScoreBadge::Elite`]: score ≥ 2000
    ///
    /// Returns `None` if the user has no reputation entry or their decayed score is
    /// below 100.
    ///
    /// This is **not** [`Self::get_tier`]. `total_score` is the sum of
    /// `rating × stake_weight` over all reviews, so this ladder rises with review
    /// count and staked amount, while the rating tier is an average and does not.
    /// The two answer different questions and routinely disagree; the ladders have
    /// different rungs on purpose (there is no Platinum score badge). See the module
    /// docs (issue #1172).
    ///
    /// Renamed from `get_badge`, which remains available as an alias.
    pub fn get_score_badge(env: Env, user: Address) -> Option<ScoreBadge> {
        let rep_key = DataKey::Reputation(user.clone());
        let reputation: Option<UserReputation> = env.storage().persistent().get(&rep_key);

        match reputation {
            Some(_rep) => {
                bump_reputation_ttl(&env, &user);
                let (score, _total_weight, _review_count) = Self::get_decayed_totals(&env, user);
                if score >= 2000 {
                    Some(ScoreBadge::Elite)
                } else if score >= 500 {
                    Some(ScoreBadge::Established)
                } else if score >= 100 {
                    Some(ScoreBadge::Rising)
                } else {
                    None
                }
            }
            None => None,
        }
    }

    /// Backwards-compatible alias of [`Self::get_score_badge`], kept so existing
    /// integrations keep working after the issue #1172 rename. Prefer
    /// `get_score_badge`, whose name states which of the two reputation ladders it
    /// reports.
    pub fn get_badge(env: Env, user: Address) -> Option<ScoreBadge> {
        Self::get_score_badge(env, user)
    }

    /// Claim staked tokens back after a lockup period. Allows reviewers to withdraw
    /// their stakes. Transfers the claimed amount from the contract back to the reviewer.
    ///
    /// Enforces a `STAKE_LOCKUP_SECONDS` wait from the timestamp of the last
    /// `submit_review` that deposited stake. This prevents flash-loan-funded
    /// governance weight inflation: an attacker who inflates `total_score` via a
    /// self-dealt high-stake review must leave the capital locked for 7 days
    /// before recovering it, making the attack economically meaningful rather
    /// than free-of-cost (issue #exploit).
    pub fn claim_stake(env: Env, reviewer: Address, amount: i128) -> Result<(), ReputationError> {
        reviewer.require_auth();
        require_not_paused(&env)?;

        let balance_key = DataKey::StakeBalance(reviewer.clone());
        let balance: i128 = env.storage().persistent().get(&balance_key).unwrap_or(0);

        if balance < amount || amount <= 0 {
            return Err(ReputationError::BelowMinStake);
        }

        // Lockup enforcement: reject claims where the stake is younger than
        // STAKE_LOCKUP_SECONDS. Pre-existing stakes without a recorded timestamp
        // (StakeLockupTs absent) are treated as if they were staked at time 0,
        // so they immediately satisfy the lockup and remain claimable — no
        // backward-compatibility breakage for stakes that predate this check.
        let lockup_key = DataKey::StakeLockupTs(reviewer.clone());
        let staked_at: u64 = env
            .storage()
            .persistent()
            .get(&lockup_key)
            .unwrap_or(0u64);
        let current_ts = env.ledger().timestamp();
        if current_ts < staked_at.saturating_add(STAKE_LOCKUP_SECONDS) {
            return Err(ReputationError::StakeLockupActive);
        }

        // Update balance
        let new_balance = balance - amount;
        if new_balance > 0 {
            env.storage().persistent().set(&balance_key, &new_balance);
        } else {
            env.storage().persistent().remove(&balance_key);
        }

        // Transfer tokens back to reviewer, using the token the stake was
        // actually deposited in (not the reputation contract's own address).
        let stake_token_key = DataKey::StakeToken(reviewer.clone());
        let stake_token: Address = env
            .storage()
            .persistent()
            .get(&stake_token_key)
            .ok_or(ReputationError::StakeTokenNotFound)?;
        let token_client = token::Client::new(&env, &stake_token);
        token_client.transfer(&env.current_contract_address(), &reviewer, &amount);

        if new_balance <= 0 {
            env.storage().persistent().remove(&stake_token_key);
            // Remove the lockup timestamp too — no stake remaining means the
            // next deposit will anchor a fresh lockup window.
            env.storage().persistent().remove(&lockup_key);
        }

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("claim")),
            (reviewer, amount),
        );

        Ok(())
    }

    /// File an appeal for a previously submitted review.
    /// Can only be filed by the reviewee within the configured grace window.
    pub fn appeal_review(
        env: Env,
        reviewer: Address,
        reviewee: Address,
        job_id: u64,
        reason: String,
    ) -> Result<(), ReputationError> {
        reviewee.require_auth();
        require_not_paused(&env)?;

        let reviews_key = DataKey::Reviews(reviewee.clone());
        let reviews: Vec<Review> = env
            .storage()
            .persistent()
            .get(&reviews_key)
            .unwrap_or(Vec::new(&env));

        let review = reviews
            .iter()
            .find(|r| r.reviewer == reviewer && r.job_id == job_id)
            .ok_or(ReputationError::ReviewNotFound)?;

        let now = env.ledger().timestamp();
        if now > review.timestamp.saturating_add(APPEAL_GRACE_WINDOW_SECONDS) {
            return Err(ReputationError::AppealWindowExpired);
        }

        let appeal_key = DataKey::ReviewAppeal(reviewer.clone(), reviewee.clone(), job_id);
        if env.storage().persistent().has(&appeal_key) {
            return Err(ReputationError::AppealAlreadyExists);
        }

        let appeal = ReviewAppeal {
            reviewer: reviewer.clone(),
            reviewee: reviewee.clone(),
            job_id,
            reason: reason.clone(),
            created_at: now,
            expires_at: review.timestamp.saturating_add(APPEAL_GRACE_WINDOW_SECONDS),
            status: AppealStatus::Pending,
        };

        env.storage().persistent().set(&appeal_key, &appeal);
        bump_review_appeal_ttl(&env, &reviewer, &reviewee, job_id);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("appealed")),
            (reviewer, reviewee, job_id, reason.clone()),
        );

        Ok(())
    }

    /// Get a paginated list of top users by average rating.
    pub fn get_leaderboard_page(env: Env, offset: u32, limit: u32) -> Vec<(Address, u64)> {
        let leaderboard_key = DataKey::Leaderboard;
        let leaderboard: Option<Vec<(Address, u64)>> =
            env.storage().instance().get(&leaderboard_key);

        let list = match leaderboard {
            Some(l) => {
                env.storage()
                    .instance()
                    .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
                l
            }
            None => return Vec::new(&env),
        };

        let mut filtered = Vec::new(&env);
        for entry in list.iter() {
            let is_banned: bool = env
                .storage()
                .instance()
                .get(&DataKey::BannedUser(entry.0.clone()))
                .unwrap_or(false);
            if !is_banned {
                filtered.push_back(entry);
            }
        }
        let list = filtered;

        let total = list.len();
        if offset >= total {
            return Vec::new(&env);
        }

        let actual_limit = if limit > 50 { 50 } else { limit };
        let end = offset.saturating_add(actual_limit);
        let end = if end > total { total } else { end };

        let mut page = Vec::new(&env);
        for i in offset..end {
            page.push_back(list.get(i).unwrap());
        }
        page
    }

    /// Get the top N users by average rating. Returns a vector of (Address, average_rating)
    /// tuples sorted by rating (highest first), up to top 50.
    /// Deprecated: use get_leaderboard_page instead.
    pub fn get_leaderboard(env: Env) -> Vec<(Address, u64)> {
        Self::get_leaderboard_page(env, 0, 50)
    }

    /// Internal function to update the leaderboard after a review is submitted.
    /// Maintains a sorted list of top 50 users by average rating.
    fn update_leaderboard(env: &Env, reviewee: &Address) {
        // Compute the user's decayed totals once. These drive both the
        // zero-score removal check (issue #785) and the average rating used for
        // ranking, so we read them here instead of paying for two passes.
        let (new_score, new_weight, _) = Self::get_decayed_totals(env, reviewee.clone());

        let leaderboard_key = DataKey::Leaderboard;
        let mut leaderboard: Vec<(Address, u64)> = env
            .storage()
            .instance()
            .get(&leaderboard_key)
            .unwrap_or(Vec::new(env));

        // Remove existing entry for this user if present
        let mut idx: u32 = 0;
        while idx < leaderboard.len() {
            if leaderboard.get(idx).unwrap().0 == *reviewee {
                leaderboard.remove(idx);
                break;
            }
            idx += 1;
        }

        // Issue #785: once a user's score and weight have fully decayed to zero,
        // their entry must not linger on the leaderboard showing a 0 — that
        // pushes legitimate active users out of the visible top list. Persist
        // the leaderboard with the stale entry dropped (the loop above already
        // removed it) and stop before re-inserting. We intentionally leave the
        // user's reputation/review records intact: those hold history (and
        // accumulator-only score such as dispute outcomes) that must survive a
        // dormant period and is unrelated to leaderboard visibility.
        if new_score == 0 && new_weight == 0 {
            env.storage().instance().set(&leaderboard_key, &leaderboard);
            env.storage()
                .instance()
                .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
            return;
        }

        let avg_rating = match Self::get_average_rating(env.clone(), reviewee.clone()) {
            Ok(rating) => rating,
            Err(_) => {
                // Reputation vanished between reads — persist the removal so a
                // stale entry is not left behind, then stop.
                env.storage().instance().set(&leaderboard_key, &leaderboard);
                env.storage()
                    .instance()
                    .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
                return;
            }
        };

        // Insert at correct position (descending by rating)
        let mut inserted = false;
        let mut pos: u32 = 0;
        while pos < leaderboard.len() {
            let rating = leaderboard.get(pos).unwrap().1;
            if avg_rating > rating {
                leaderboard.insert(pos, (reviewee.clone(), avg_rating));
                inserted = true;
                break;
            }
            pos += 1;
        }

        if !inserted {
            leaderboard.push_back((reviewee.clone(), avg_rating));
        }

        env.storage().instance().set(&leaderboard_key, &leaderboard);
        env.storage()
            .instance()
            .extend_ttl(MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO);
    }

    /// Resolve an existing review appeal.
    /// Admin can remove the review or dismiss the appeal.
    pub fn admin_resolve_appeal(
        env: Env,
        admin: Address,
        reviewer: Address,
        reviewee: Address,
        job_id: u64,
        remove: bool,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        require_not_paused(&env)?;
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }

        let appeal_key = DataKey::ReviewAppeal(reviewer.clone(), reviewee.clone(), job_id);
        let mut appeal: ReviewAppeal = env
            .storage()
            .persistent()
            .get(&appeal_key)
            .ok_or(ReputationError::AppealNotFound)?;

        if appeal.status != AppealStatus::Pending {
            return Err(ReputationError::AppealAlreadyResolved);
        }

        if remove {
            let reviews_key = DataKey::Reviews(reviewee.clone());
            let mut reviews: Vec<Review> = env
                .storage()
                .persistent()
                .get(&reviews_key)
                .unwrap_or(Vec::new(&env));

            let review_index = reviews
                .iter()
                .position(|r| r.reviewer == reviewer && r.job_id == job_id)
                .ok_or(ReputationError::ReviewNotFound)?;

            let removed_review = reviews.get(review_index as u32).unwrap();
            let removed_weight = if removed_review.stake_weight > 0 {
                removed_review.stake_weight as u64
            } else {
                1
            };
            let removed_score = removed_weight.saturating_mul(removed_review.rating as u64);

            reviews.remove(review_index as u32);
            env.storage().persistent().set(&reviews_key, &reviews);
            bump_reviews_ttl(&env, &reviewee);

            let review_exists_key =
                DataKey::ReviewExists(reviewer.clone(), reviewee.clone(), job_id);
            if env.storage().persistent().has(&review_exists_key) {
                env.storage().persistent().remove(&review_exists_key);
            }

            let rep_key = DataKey::Reputation(reviewee.clone());
            let mut reputation: UserReputation = env
                .storage()
                .persistent()
                .get(&rep_key)
                .unwrap_or(UserReputation {
                    user: reviewee.clone(),
                    total_score: 0,
                    total_weight: 0,
                    review_count: 0,
                    last_updated_ts: env.ledger().timestamp() as u32,
                });

            apply_lazy_decay(&env, &mut reputation);

            reputation.total_score = reputation.total_score.saturating_sub(removed_score);
            reputation.total_weight = reputation.total_weight.saturating_sub(removed_weight);
            reputation.review_count = reputation.review_count.saturating_sub(1);
            reputation.last_updated_ts = env.ledger().timestamp() as u32;
            env.storage().persistent().set(&rep_key, &reputation);
            bump_reputation_ttl(&env, &reviewee);

            appeal.status = AppealStatus::ReviewRemoved;
        } else {
            appeal.status = AppealStatus::Dismissed;
        }

        env.storage().persistent().set(&appeal_key, &appeal);
        bump_review_appeal_ttl(&env, &reviewer, &reviewee, job_id);

        env.events().publish(
            (symbol_short!("reput"), symbol_short!("ap_reslv")),
            (admin, reviewer, reviewee, job_id, remove),
        );

        Ok(())
    }

    /// Admin: remove a review by index from a user's stored reviews.
    /// - Requires admin auth (registered signer).
    /// - Removes the review at `review_index` from `DataKey::Reviews(user)`.
    /// - Does NOT recompute the aggregate; totals are recomputed lazily on read.
    /// - Emits an event for audit: ("reput", "review_removed") -> (user, review_index, removed_by)
    pub fn admin_remove_review(
        env: Env,
        admin: Address,
        user: Address,
        review_index: u32,
    ) -> Result<(), ReputationError> {
        admin.require_auth();
        require_not_paused(&env)?;
        if !is_signer(&env, &admin) {
            return Err(ReputationError::NotAdmin);
        }

        let reviews_key = DataKey::Reviews(user.clone());
        let mut reviews: Vec<Review> = env
            .storage()
            .persistent()
            .get(&reviews_key)
            .unwrap_or(Vec::new(&env));

        if review_index >= reviews.len() {
            return Err(ReputationError::ReviewNotFound);
        }

        let removed = reviews.get(review_index).unwrap();
        let reviewer = removed.reviewer.clone();
        let job_id = removed.job_id;

        reviews.remove(review_index);
        env.storage().persistent().set(&reviews_key, &reviews);
        bump_reviews_ttl(&env, &user);

        let review_exists_key = DataKey::ReviewExists(reviewer.clone(), user.clone(), job_id);
        if env.storage().persistent().has(&review_exists_key) {
            env.storage().persistent().remove(&review_exists_key);
        }

        // Resolve any pending appeal on the removed review so both removal
        // paths (admin_remove_review and admin_resolve_appeal) leave
        // consistent appeal state (#981).
        let appeal_key = DataKey::ReviewAppeal(reviewer.clone(), user.clone(), job_id);
        if let Some(mut appeal) = env
            .storage()
            .persistent()
            .get::<DataKey, ReviewAppeal>(&appeal_key)
        {
            if appeal.status == AppealStatus::Pending {
                appeal.status = AppealStatus::ReviewRemoved;
                env.storage().persistent().set(&appeal_key, &appeal);
                bump_review_appeal_ttl(&env, &reviewer, &user, job_id);
            }
        }

        // Do NOT update legacy accumulators here; recomputation is lazy on next read.

        // NOTE: Soroban symbol literals have a hard 9-character limit.
        // Keep event topic symbols <= 9 chars.
        env.events().publish(
            (symbol_short!("reput"), symbol_short!("rev_rm")),
            (user, review_index, admin),
        );

        Ok(())
    }

    pub fn get_review_appeal(
        env: Env,
        reviewer: Address,
        reviewee: Address,
        job_id: u64,
    ) -> Result<ReviewAppeal, ReputationError> {
        let key = DataKey::ReviewAppeal(reviewer.clone(), reviewee.clone(), job_id);
        let appeal: ReviewAppeal = env
            .storage()
            .persistent()
            .get(&key)
            .ok_or(ReputationError::AppealNotFound)?;
        bump_review_appeal_ttl(&env, &reviewer, &reviewee, job_id);
        Ok(appeal)
    }
}

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, testutils::Ledger, vec, Env};

    fn seed_review_state(
        env: &Env,
        contract_id: &Address,
        reviewer: &Address,
        reviewee: &Address,
        job_id: u64,
        rating: u32,
    ) {
        let review = Review {
            reviewer: reviewer.clone(),
            reviewee: reviewee.clone(),
            job_id,
            rating,
            comment: String::from_str(env, "seed review"),
            stake_weight: MIN_REVIEW_STAKE_DEFAULT,
            timestamp: env.ledger().timestamp(),
        };

        let reviews = vec![env, review];
        env.as_contract(contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::Reviews(reviewee.clone()), &reviews);
            env.storage().persistent().set(
                &DataKey::ReviewExists(reviewer.clone(), reviewee.clone(), job_id),
                &true,
            );
            env.storage().persistent().set(
                &DataKey::Reputation(reviewee.clone()),
                &UserReputation {
                    user: reviewee.clone(),
                    total_score: (rating as u64) * (MIN_REVIEW_STAKE_DEFAULT as u64),
                    total_weight: MIN_REVIEW_STAKE_DEFAULT as u64,
                    review_count: 1,
                    last_updated_ts: env.ledger().timestamp() as u32,
                },
            );
        });
    }

    #[test]
    fn test_appeal_and_admin_remove_review_flow() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        let reviewer = Address::generate(&env);
        let reviewee = Address::generate(&env);
        seed_review_state(&env, &contract_id, &reviewer, &reviewee, 42, 1);

        client.appeal_review(
            &reviewer,
            &reviewee,
            &42,
            &String::from_str(&env, "malicious 1-star review"),
        );
        let appeal = client.get_review_appeal(&reviewer, &reviewee, &42);
        assert_eq!(appeal.status, AppealStatus::Pending);

        client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &42, &true);
        let resolved = client.get_review_appeal(&reviewer, &reviewee, &42);
        assert_eq!(resolved.status, AppealStatus::ReviewRemoved);

        assert_eq!(client.get_reviews(&reviewee).len(), 0);
        let rep = client.get_reputation(&reviewee);
        assert_eq!(rep.review_count, 0);
        assert_eq!(rep.total_score, 0);
        assert_eq!(rep.total_weight, 0);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #19)")]
    fn test_appeal_outside_window_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);
        let reviewer = Address::generate(&env);
        let reviewee = Address::generate(&env);

        seed_review_state(&env, &contract_id, &reviewer, &reviewee, 99, 2);
        let now = env.ledger().timestamp();
        env.ledger()
            .with_mut(|l| l.timestamp = now + APPEAL_GRACE_WINDOW_SECONDS + 1);

        client.appeal_review(
            &reviewer,
            &reviewee,
            &99,
            &String::from_str(&env, "late appeal"),
        );
    }

    // Seeds both the `Reputation` accumulator and a matching `Reviews` entry so
    // that `get_decayed_totals` (used by `get_score_badge`) reports the same score as
    // the raw `total_score` when no decay has elapsed (issue #976).
    fn seed_reputation_with_review(
        env: &Env,
        contract_id: &Address,
        user: &Address,
        total_score: u64,
        total_weight: u64,
        review_count: u32,
    ) {
        env.as_contract(contract_id, || {
            env.storage().persistent().set(
                &DataKey::Reputation(user.clone()),
                &UserReputation {
                    user: user.clone(),
                    total_score,
                    total_weight,
                    review_count,
                    last_updated_ts: 0,
                },
            );
            env.storage().persistent().set(
                &DataKey::Reviews(user.clone()),
                &Vec::from_array(
                    env,
                    [Review {
                        reviewer: user.clone(),
                        reviewee: user.clone(),
                        job_id: 0,
                        rating: total_score as u32,
                        comment: String::from_str(env, ""),
                        stake_weight: 1,
                        timestamp: 0,
                    }],
                ),
            );
        });
    }

    #[test]
    fn test_get_badge() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let user = Address::generate(&env);

        // Test no badge for user with no reputation
        assert_eq!(client.get_score_badge(&user), None);

        // Test Rising badge (score >= 100)
        seed_reputation_with_review(&env, &contract_id, &user, 100, 10, 1);
        assert_eq!(client.get_score_badge(&user), Some(ScoreBadge::Rising));

        // Test Established badge (score >= 500)
        seed_reputation_with_review(&env, &contract_id, &user, 500, 50, 5);
        assert_eq!(client.get_score_badge(&user), Some(ScoreBadge::Established));

        // Test Elite badge (score >= 2000)
        seed_reputation_with_review(&env, &contract_id, &user, 2000, 200, 20);
        assert_eq!(client.get_score_badge(&user), Some(ScoreBadge::Elite));

        // Test no badge for score < 100
        seed_reputation_with_review(&env, &contract_id, &user, 99, 10, 1);
        assert_eq!(client.get_score_badge(&user), None);
    }

    /// #1172 — the deprecated `get_badge` name must keep answering exactly like
    /// `get_score_badge` so existing integrations are unaffected by the rename.
    #[test]
    fn test_get_badge_alias_matches_get_score_badge() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let user = Address::generate(&env);
        assert_eq!(client.get_badge(&user), client.get_score_badge(&user));

        for (score, weight, count) in [(99u64, 10u64, 1u32), (100, 10, 1), (500, 50, 5), (2000, 200, 20)] {
            seed_reputation_with_review(&env, &contract_id, &user, score, weight, count);
            assert_eq!(client.get_badge(&user), client.get_score_badge(&user));
        }
    }

    /// Seed `count` reviews of the given rating and stake weight, so a test can
    /// control `total_score` (Σ rating × weight) and `total_weight` (Σ weight)
    /// independently of each other.
    fn seed_reviews(
        env: &Env,
        contract_id: &Address,
        user: &Address,
        rating: u32,
        stake_weight: i128,
        count: u32,
    ) {
        env.as_contract(contract_id, || {
            let mut reviews = Vec::new(env);
            for _ in 0..count {
                reviews.push_back(Review {
                    reviewer: user.clone(),
                    reviewee: user.clone(),
                    job_id: 0,
                    rating,
                    comment: String::from_str(env, ""),
                    stake_weight,
                    timestamp: 0,
                });
            }
            env.storage().persistent().set(
                &DataKey::Reputation(user.clone()),
                &UserReputation {
                    user: user.clone(),
                    total_score: 0,
                    total_weight: 0,
                    review_count: count,
                    last_updated_ts: 0,
                },
            );
            env.storage()
                .persistent()
                .set(&DataKey::Reviews(user.clone()), &reviews);
        });
    }

    /// #1172 — the score badge and the rating tier are separate ladders. A user with
    /// one small perfect review ranks high on quality and nowhere on volume; a user
    /// with lots of mediocre, heavily-staked reviews does the opposite. This pins
    /// that divergence so the two are never "fixed" into agreement.
    #[test]
    fn test_score_badge_and_rating_tier_are_independent_ladders() {
        let env = Env::default();
        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        // One 5★ review of weight 1: average_rating = 5 * 100 / 1 = 500 -> Gold
        // tier, but total_score = 5 -> below the score ladder's first rung.
        let sparse = Address::generate(&env);
        seed_reviews(&env, &contract_id, &sparse, 5, 1, 1);
        assert_eq!(client.get_tier(&sparse), ReputationTier::Gold);
        assert_eq!(client.get_score_badge(&sparse), None);

        // Ten 2★ reviews of weight 100: total_score = 2000 -> Elite badge, while
        // average_rating = 2000 * 100 / 1000 = 200 -> only Bronze tier.
        let prolific = Address::generate(&env);
        seed_reviews(&env, &contract_id, &prolific, 2, 100, 10);
        assert_eq!(client.get_score_badge(&prolific), Some(ScoreBadge::Elite));
        assert_eq!(client.get_tier(&prolific), ReputationTier::Bronze);
    }

    #[test]
    fn test_get_badge_matches_get_tier_after_full_decay() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        // 100% decay rate per year so the score fully decays after one year.
        client.initialize(&vec![&env, admin.clone()], &1, &100);

        let user = Address::generate(&env);
        env.ledger().with_mut(|l| l.timestamp = 0);
        seed_reputation_with_review(&env, &contract_id, &user, 2000, 200, 20);

        // Freshly earned: both views agree the user has standing.
        assert_eq!(client.get_score_badge(&user), Some(ScoreBadge::Elite));
        assert_ne!(client.get_tier(&user), ReputationTier::None);

        // Advance a full year so the review fully decays.
        env.ledger()
            .with_mut(|l| l.timestamp = ONE_YEAR_IN_SECONDS);

        assert_eq!(client.get_score_badge(&user), None);
        assert_eq!(client.get_tier(&user), ReputationTier::None);
    }

    #[test]
    fn test_apply_dispute_outcome() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        let dispute_contract = Address::generate(&env);
        client.set_dispute_contract(&admin, &dispute_contract);

        let user = Address::generate(&env);

        // Initialize user with some reputation
        env.as_contract(&contract_id, || {
            env.storage().persistent().set(
                &DataKey::Reputation(user.clone()),
                &UserReputation {
                    user: user.clone(),
                    total_score: 500,
                    total_weight: 50,
                    review_count: 5,
                    last_updated_ts: 0,
                },
            );
        });

        // mock_all_auths() makes dispute_contract.require_auth() pass; no as_contract needed.
        // apply_dispute_outcome reads/writes DataKey::Reputation; verify via direct storage read.

        // Test Won outcome (+50 points): 500 → 550
        client.apply_dispute_outcome(&user, &DisputeOutcome::Won);
        env.as_contract(&contract_id, || {
            let rep: UserReputation = env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap();
            assert_eq!(rep.total_score, 550);
        });

        // Test Lost outcome (-100 points): 550 → 450
        client.apply_dispute_outcome(&user, &DisputeOutcome::Lost);
        env.as_contract(&contract_id, || {
            let rep: UserReputation = env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap();
            assert_eq!(rep.total_score, 450);
        });

        // Test MaliciousFiling outcome (-250 points): 450 → 200
        client.apply_dispute_outcome(&user, &DisputeOutcome::MaliciousFiling);
        env.as_contract(&contract_id, || {
            let rep: UserReputation = env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap();
            assert_eq!(rep.total_score, 200);
        });

        // Test that score saturates at 0 (never goes negative)
        env.as_contract(&contract_id, || {
            env.storage().persistent().set(
                &DataKey::Reputation(user.clone()),
                &UserReputation {
                    user: user.clone(),
                    total_score: 100,
                    total_weight: 10,
                    review_count: 1,
                    last_updated_ts: 0,
                },
            );
        });
        client.apply_dispute_outcome(&user, &DisputeOutcome::MaliciousFiling);
        env.as_contract(&contract_id, || {
            let rep: UserReputation = env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap();
            assert_eq!(rep.total_score, 0);
        });
    }

    #[test]
    #[should_panic]
    fn test_apply_dispute_outcome_unauthorized() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        // Set an escrow contract as the dispute contract so the auth check has a real address.
        let dispute_contract = Address::generate(&env);
        client.set_dispute_contract(&admin, &dispute_contract);

        let user = Address::generate(&env);

        // Drop all auth mocks so that dispute_contract.require_auth() fails when called
        // without the dispute_contract's authorization — should panic.
        env.set_auths(&[]);
        client.apply_dispute_outcome(&user, &DisputeOutcome::Won);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #9)")]
    fn test_apply_dispute_outcome_no_dispute_contract() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        let user = Address::generate(&env);

        // Try to call without dispute contract set
        client.apply_dispute_outcome(&user, &DisputeOutcome::Won);
    }

    #[test]
    fn test_admin_remove_review_by_index() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        let reviewee = Address::generate(&env);
        let reviewer1 = Address::generate(&env);
        let reviewer2 = Address::generate(&env);
        let reviewer3 = Address::generate(&env);

        // Create three reviews: ratings 5,3,4 each with MIN_REVIEW_STAKE_DEFAULT weight
        let r1 = Review {
            reviewer: reviewer1.clone(),
            reviewee: reviewee.clone(),
            job_id: 1,
            rating: 5,
            comment: String::from_str(&env, "r1"),
            stake_weight: MIN_REVIEW_STAKE_DEFAULT,
            timestamp: env.ledger().timestamp(),
        };
        let r2 = Review {
            reviewer: reviewer2.clone(),
            reviewee: reviewee.clone(),
            job_id: 2,
            rating: 3,
            comment: String::from_str(&env, "r2"),
            stake_weight: MIN_REVIEW_STAKE_DEFAULT,
            timestamp: env.ledger().timestamp(),
        };
        let r3 = Review {
            reviewer: reviewer3.clone(),
            reviewee: reviewee.clone(),
            job_id: 3,
            rating: 4,
            comment: String::from_str(&env, "r3"),
            stake_weight: MIN_REVIEW_STAKE_DEFAULT,
            timestamp: env.ledger().timestamp(),
        };

        let reviews = vec![&env, r1, r2, r3];
        // Set persisted reviews and review_exists flags and a legacy reputation record
        env.as_contract(&contract_id, || {
            env.storage()
                .persistent()
                .set(&DataKey::Reviews(reviewee.clone()), &reviews);

            env.storage().persistent().set(
                &DataKey::ReviewExists(reviewer1.clone(), reviewee.clone(), 1),
                &true,
            );
            env.storage().persistent().set(
                &DataKey::ReviewExists(reviewer2.clone(), reviewee.clone(), 2),
                &true,
            );
            env.storage().persistent().set(
                &DataKey::ReviewExists(reviewer3.clone(), reviewee.clone(), 3),
                &true,
            );

            // Legacy accumulator: sum ratings * weight
            let total_score = (5u64 + 3u64 + 4u64) * (MIN_REVIEW_STAKE_DEFAULT as u64);
            let total_weight = 3u64 * (MIN_REVIEW_STAKE_DEFAULT as u64);
            env.storage().persistent().set(
                &DataKey::Reputation(reviewee.clone()),
                &UserReputation {
                    user: reviewee.clone(),
                    total_score,
                    total_weight,
                    review_count: 3,
                    last_updated_ts: env.ledger().timestamp() as u32,
                },
            );
        });

        // Sanity check: average rating should be 400 ( (12/3)*100 )
        let before = client.get_average_rating(&reviewee);
        assert_eq!(before, 400);

        // Admin removes index 1 (the middle review with rating 3)
        client.admin_remove_review(&admin, &reviewee, &1);

        // Reviews length should be 2 now
        assert_eq!(client.get_reviews(&reviewee).len(), 2);

        // New average should be ((5+4)/2)*100 = 450
        let after = client.get_average_rating(&reviewee);
        assert_eq!(after, 450);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #14)")]
    fn test_admin_remove_review_unauthorized_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        client.initialize(&vec![&env, admin.clone()], &1, &0);

        let non_admin = Address::generate(&env);
        let reviewee = Address::generate(&env);

        // Call without being a registered signer — should panic with NotAdmin
        client.admin_remove_review(&non_admin, &reviewee, &0);
    }

    #[test]
    fn test_dispute_outcome_decay_behavior() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, ReputationContract);
        let client = ReputationContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        // Initialize with default decay rate of 10% (10u32)
        client.initialize(&vec![&env, admin.clone()], &1, &10);

        let dispute_contract = Address::generate(&env);
        client.set_dispute_contract(&admin, &dispute_contract);

        let user = Address::generate(&env);

        // 1. New user has their first reputation event via dispute outcome.
        // Timestamp is 500,000.
        env.ledger().with_mut(|l| l.timestamp = 500_000);
        client.apply_dispute_outcome(&user, &DisputeOutcome::Won);

        // Verify the user's reputation was created, total_score = 50, and last_updated_ts is 500,000.
        let rep_before: UserReputation = env.as_contract(&contract_id, || {
            env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap()
        });
        assert_eq!(rep_before.total_score, 50);
        assert_eq!(rep_before.last_updated_ts, 500_000);

        // 2. Advance time by 1 year (31,536,000 seconds) so that 10% decay applies.
        env.ledger().with_mut(|l| l.timestamp = 500_000 + 31_536_000);

        // Perform an unrelated action or a second dispute outcome to trigger lazy decay.
        // We will call apply_dispute_outcome with Won (+50) again.
        // With 10% decay, the original 50 score should decay to 45.
        // Then +50 is added, resulting in 95.
        client.apply_dispute_outcome(&user, &DisputeOutcome::Won);

        let rep_after: UserReputation = env.as_contract(&contract_id, || {
            env.storage().persistent()
                .get(&DataKey::Reputation(user.clone()))
                .unwrap()
        });
        assert_eq!(rep_after.total_score, 95);
        assert_eq!(rep_after.last_updated_ts, 500_000 + 31_536_000);
    }
}
