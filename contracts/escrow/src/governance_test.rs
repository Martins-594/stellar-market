//! Tests for reputation-weighted on-chain governance (issue #899).
//!
//! Voting weight is sourced from a controllable [`MockReputation`] contract that
//! implements the same `get_gov_weight(user) -> (score, last_change_ts)`
//! interface the real reputation contract exposes. The mock lets each test set a
//! user's score *and* the timestamp of their last reputation change, which is
//! exactly what the snapshot-safety / anti-gaming property hinges on.

use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Ledger},
    Address, Env,
};

use crate::governance::{GovError, GovProposalStatus, VoteSupport};
use crate::{AdminAction, EscrowContract, EscrowContractClient, EscrowError};

// ============================================================
// Mock reputation contract
// ============================================================

#[contract]
pub struct MockReputation;

#[contractimpl]
impl MockReputation {
    /// Set a user's `(score, last_change_ts)` used by governance snapshots.
    pub fn set(env: Env, user: Address, score: u64, last_change_ts: u64) {
        env.storage()
            .persistent()
            .set(&user, &(score, last_change_ts));
    }

    /// Mirrors `stellar-market-reputation::get_gov_weight`.
    pub fn get_gov_weight(env: Env, user: Address) -> (u64, u64) {
        env.storage().persistent().get(&user).unwrap_or((0, 0))
    }
}

// ============================================================
// Test harness
// ============================================================

const VOTING_PERIOD: u64 = 3 * 24 * 3600; // 3 days
const TIMELOCK: u64 = 24 * 3600; // 1 day
const GRACE: u64 = 3 * 24 * 3600; // 3 days
const QUORUM: u128 = 100;
const PASS_BPS: u32 = 5000; // simple majority (> 50% of decisive votes)

struct Ctx<'a> {
    env: Env,
    escrow: EscrowContractClient<'a>,
    rep_id: Address,
    signer: Address,
}

/// Deploy escrow + mock reputation, initialise the multisig, and enable
/// governance with the standard test parameters.
fn setup() -> Ctx<'static> {
    let env = Env::default();
    env.mock_all_auths();
    // A generous ledger timestamp so we can freely rewind reputation changes to
    // "before the snapshot" without underflowing.
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    let escrow_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &escrow_id);
    let rep_id = env.register_contract(None, MockReputation);

    let signer = Address::generate(&env);
    let treasury = Address::generate(&env);
    let signers = soroban_sdk::vec![&env, signer.clone()];
    // Start fee at 100 bps so a governance change to 250 is observable.
    escrow.initialize(&signers, &1, &treasury, &100, &604_800);

    escrow.configure_governance(
        &signer,
        &rep_id,
        &VOTING_PERIOD,
        &TIMELOCK,
        &GRACE,
        &QUORUM,
        &PASS_BPS,
        &0, // min_proposer_weight
    );

    Ctx {
        env,
        escrow,
        rep_id,
        signer,
    }
}

/// Register a voter in the mock reputation contract with `score` last changed at
/// `last_change_ts`, and return the address.
fn voter_with(ctx: &Ctx, score: u64, last_change_ts: u64) -> Address {
    let a = Address::generate(&ctx.env);
    set_rep(ctx, &a, score, last_change_ts);
    a
}

fn set_rep(ctx: &Ctx, who: &Address, score: u64, last_change_ts: u64) {
    let rep = MockReputationClient::new(&ctx.env, &ctx.rep_id);
    rep.set(who, &score, &last_change_ts);
}

fn advance(ctx: &Ctx, secs: u64) {
    ctx.env.ledger().with_mut(|l| l.timestamp += secs);
}

// ============================================================
// Happy path: a proposal passes and takes effect
// ============================================================

#[test]
fn proposal_passes_and_applies_parameter_change() {
    let ctx = setup();
    // Two established voters (reputation set well before the snapshot).
    let a = voter_with(&ctx, 80, 500_000);
    let b = voter_with(&ctx, 40, 500_000);

    assert_eq!(ctx.escrow.get_fee_bps(), 100);

    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));

    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    ctx.escrow.cast_vote(&b, &id, &VoteSupport::For);

    // Close voting, then finalize.
    advance(&ctx, VOTING_PERIOD + 1);
    let status = ctx.escrow.finalize_governance(&id);
    assert_eq!(status, GovProposalStatus::Queued);

    // Time-lock must elapse before execution.
    assert_eq!(
        ctx.escrow.try_execute_governance(&id),
        Err(Ok(GovError::TimelockActive))
    );
    advance(&ctx, TIMELOCK + 1);
    ctx.escrow.execute_governance(&id);

    assert_eq!(ctx.escrow.get_fee_bps(), 250);
    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.status, GovProposalStatus::Executed);
    assert_eq!(p.for_weight, 120);
}

#[test]
fn set_treasury_via_governance() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let new_treasury = Address::generate(&ctx.env);

    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetTreasury(new_treasury.clone()));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);

    advance(&ctx, VOTING_PERIOD + 1);
    ctx.escrow.finalize_governance(&id);
    advance(&ctx, TIMELOCK + 1);
    ctx.escrow.execute_governance(&id);

    assert_eq!(ctx.escrow.get_treasury(), Some(new_treasury));
}

// ============================================================
// Failing quorum / threshold
// ============================================================

#[test]
fn proposal_below_quorum_is_defeated_and_not_applied() {
    let ctx = setup();
    // Total participation 50 < QUORUM (100).
    let a = voter_with(&ctx, 50, 500_000);

    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);

    advance(&ctx, VOTING_PERIOD + 1);
    let status = ctx.escrow.finalize_governance(&id);
    assert_eq!(status, GovProposalStatus::Defeated);

    // A defeated proposal cannot be executed and the fee is unchanged.
    assert_eq!(
        ctx.escrow.try_execute_governance(&id),
        Err(Ok(GovError::NotQueued))
    );
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

#[test]
fn proposal_below_threshold_is_defeated() {
    let ctx = setup();
    // Quorum met (150), but `for` (60) is not a majority of decisive (150).
    let a = voter_with(&ctx, 60, 500_000);
    let b = voter_with(&ctx, 90, 500_000);

    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    ctx.escrow.cast_vote(&b, &id, &VoteSupport::Against);

    advance(&ctx, VOTING_PERIOD + 1);
    assert_eq!(
        ctx.escrow.finalize_governance(&id),
        GovProposalStatus::Defeated
    );
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

#[test]
fn abstain_counts_for_quorum_not_threshold() {
    let ctx = setup();
    // for=60, against=0, abstain=60 -> decisive=60 all `for` (passes threshold),
    // participation=120 >= quorum. Abstain lifted it over quorum without
    // affecting the for/against ratio.
    let a = voter_with(&ctx, 60, 500_000);
    let b = voter_with(&ctx, 60, 500_000);

    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    ctx.escrow.cast_vote(&b, &id, &VoteSupport::Abstain);

    advance(&ctx, VOTING_PERIOD + 1);
    assert_eq!(
        ctx.escrow.finalize_governance(&id),
        GovProposalStatus::Queued
    );
}

// ============================================================
// Snapshot safety / anti-gaming (core property)
// ============================================================

#[test]
fn reputation_gained_after_snapshot_cannot_vote() {
    let ctx = setup();
    let established = voter_with(&ctx, 150, 500_000);

    // Open the proposal; snapshot_ts is captured now.
    let id = ctx
        .escrow
        .propose_governance(&established, &AdminAction::SetFeeBps(250));
    let snapshot_ts = ctx.escrow.get_governance_proposal(&id).unwrap().snapshot_ts;

    // Attacker acquires reputation AFTER the snapshot (last_change_ts > snapshot).
    let attacker = Address::generate(&ctx.env);
    set_rep(&ctx, &attacker, 10_000, snapshot_ts + 1);

    assert_eq!(
        ctx.escrow
            .try_cast_vote(&attacker, &id, &VoteSupport::Against),
        Err(Ok(GovError::WeightChangedAfterSnapshot))
    );

    // The established voter (unchanged since before snapshot) votes fine.
    ctx.escrow.cast_vote(&established, &id, &VoteSupport::For);
    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.for_weight, 150);
    assert_eq!(p.against_weight, 0);
}

#[test]
fn adversarial_rapid_reputation_acquisition_is_ignored() {
    let ctx = setup();
    let proposer = voter_with(&ctx, 200, 400_000);
    let honest = voter_with(&ctx, 200, 400_000);

    let id = ctx
        .escrow
        .propose_governance(&proposer, &AdminAction::SetFeeBps(490));
    let snapshot_ts = ctx.escrow.get_governance_proposal(&id).unwrap().snapshot_ts;

    ctx.escrow.cast_vote(&honest, &id, &VoteSupport::For);

    // Whale spins up huge reputation mid-vote to flip the result.
    let whale = Address::generate(&ctx.env);
    advance(&ctx, 3600);
    set_rep(&ctx, &whale, 1_000_000, snapshot_ts + 3600);

    assert_eq!(
        ctx.escrow.try_cast_vote(&whale, &id, &VoteSupport::Against),
        Err(Ok(GovError::WeightChangedAfterSnapshot))
    );

    advance(&ctx, VOTING_PERIOD);
    assert_eq!(
        ctx.escrow.finalize_governance(&id),
        GovProposalStatus::Queued
    );
    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.against_weight, 0); // whale weight never counted
}

#[test]
fn zero_weight_account_cannot_vote() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));

    let nobody = Address::generate(&ctx.env); // never registered -> (0, 0)
    assert_eq!(
        ctx.escrow.try_cast_vote(&nobody, &id, &VoteSupport::For),
        Err(Ok(GovError::NoVotingPower))
    );
}

#[test]
fn cannot_vote_twice() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    assert_eq!(
        ctx.escrow.try_cast_vote(&a, &id, &VoteSupport::Against),
        Err(Ok(GovError::AlreadyVoted))
    );
}

// ============================================================
// Delegation
// ============================================================

#[test]
fn delegated_weight_applies_to_delegate_side() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);

    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));

    // delegator delegates to `delegate`.
    ctx.escrow.delegate(&delegator, &delegate);
    // A delegator may not vote directly once delegated away.
    assert_eq!(
        ctx.escrow
            .try_cast_vote(&delegator, &id, &VoteSupport::Against),
        Err(Ok(GovError::DelegatedAway))
    );

    // Delegate votes For, then pulls in the delegator's weight (same side).
    ctx.escrow.cast_vote(&delegate, &id, &VoteSupport::For);
    ctx.escrow.cast_delegated_vote(&delegate, &id, &delegator);

    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.for_weight, 130); // 40 + 90
    // The delegator's own snapshot weight (90) was used.
    let r = ctx.escrow.get_vote_receipt(&id, &delegator).unwrap();
    assert_eq!(r.weight, 90);
    assert!(r.via_delegate);
    assert_eq!(r.support, VoteSupport::For);
}

#[test]
fn changing_delegation_midvote_does_not_alter_cast_votes() {
    let ctx = setup();
    let delegate_a = voter_with(&ctx, 30, 500_000);
    let delegate_b = voter_with(&ctx, 30, 500_000);
    let delegator = voter_with(&ctx, 100, 500_000);

    let id = ctx
        .escrow
        .propose_governance(&delegate_a, &AdminAction::SetFeeBps(250));

    // Delegator -> A. A votes For and casts the delegated weight.
    ctx.escrow.delegate(&delegator, &delegate_a);
    ctx.escrow.cast_vote(&delegate_a, &id, &VoteSupport::For);
    ctx.escrow.cast_delegated_vote(&delegate_a, &id, &delegator);
    assert_eq!(ctx.escrow.get_governance_proposal(&id).unwrap().for_weight, 130);

    // Delegator re-delegates to B mid-vote. B votes Against and tries to pull the
    // same weight — it is already locked by the delegator's receipt.
    ctx.escrow.delegate(&delegator, &delegate_b);
    ctx.escrow.cast_vote(&delegate_b, &id, &VoteSupport::Against);
    assert_eq!(
        ctx.escrow
            .try_cast_delegated_vote(&delegate_b, &id, &delegator),
        Err(Ok(GovError::AlreadyVoted))
    );

    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    // Delegator's 100 stayed on the For side; only B's own 30 is Against.
    assert_eq!(p.for_weight, 130);
    assert_eq!(p.against_weight, 30);
}

#[test]
fn delegate_must_vote_before_pulling_delegated_weight() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));

    ctx.escrow.delegate(&delegator, &delegate);
    assert_eq!(
        ctx.escrow
            .try_cast_delegated_vote(&delegate, &id, &delegator),
        Err(Ok(GovError::DelegateHasNotVoted))
    );
}

#[test]
fn delegation_is_single_hop() {
    // A -> B -> C. C votes with its own weight and pulls in B's weight (so B's
    // receipt is via_delegate). B, as A's delegate, must NOT be able to forward
    // A's weight on again — delegation is capped at one hop.
    let ctx = setup();
    let c = voter_with(&ctx, 30, 500_000);
    let b = voter_with(&ctx, 30, 500_000);
    let a = voter_with(&ctx, 100, 500_000);
    let id = ctx.escrow.propose_governance(&c, &AdminAction::SetFeeBps(250));

    ctx.escrow.delegate(&a, &b);
    ctx.escrow.delegate(&b, &c);

    // C votes directly, then exercises B's delegated weight onto C's side.
    ctx.escrow.cast_vote(&c, &id, &VoteSupport::For);
    ctx.escrow.cast_delegated_vote(&c, &id, &b);
    let b_receipt = ctx.escrow.get_vote_receipt(&id, &b).unwrap();
    assert!(b_receipt.via_delegate);

    // B now holds a receipt, but it came via delegation — B cannot pass A's
    // weight further down the chain.
    assert_eq!(
        ctx.escrow.try_cast_delegated_vote(&b, &id, &a),
        Err(Ok(GovError::DelegateNotDirect))
    );

    // A's 100 never entered the tally; only C's 30 + B's 30 did.
    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.for_weight, 60);
}

#[test]
fn non_delegate_cannot_cast_delegated_vote() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);
    let stranger = voter_with(&ctx, 40, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));

    ctx.escrow.delegate(&delegator, &delegate);
    ctx.escrow.cast_vote(&stranger, &id, &VoteSupport::For);
    // `stranger` is not the delegator's delegate.
    assert_eq!(
        ctx.escrow
            .try_cast_delegated_vote(&stranger, &id, &delegator),
        Err(Ok(GovError::NotDelegate))
    );
}

#[test]
fn delegated_weight_is_snapshot_safe() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));
    let snapshot_ts = ctx.escrow.get_governance_proposal(&id).unwrap().snapshot_ts;

    ctx.escrow.delegate(&delegator, &delegate);
    ctx.escrow.cast_vote(&delegate, &id, &VoteSupport::For);

    // Delegator's reputation changes after the snapshot -> its weight is rejected.
    set_rep(&ctx, &delegator, 5_000, snapshot_ts + 1);
    assert_eq!(
        ctx.escrow
            .try_cast_delegated_vote(&delegate, &id, &delegator),
        Err(Ok(GovError::WeightChangedAfterSnapshot))
    );
}

#[test]
fn undelegate_removes_delegation_and_restores_direct_voting() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);

    // Initial state: no delegation set
    assert_eq!(ctx.escrow.get_delegate(&delegator), None);

    // 1. Delegate away
    ctx.escrow.delegate(&delegator, &delegate);
    assert_eq!(ctx.escrow.get_delegate(&delegator), Some(delegate.clone()));

    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));

    // Delegator cannot vote directly while delegated
    assert_eq!(
        ctx.escrow
            .try_cast_vote(&delegator, &id, &VoteSupport::Against),
        Err(Ok(GovError::DelegatedAway))
    );

    // 2. Undelegate
    ctx.escrow.undelegate(&delegator);

    // Verify delegation is completely cleared
    assert_eq!(ctx.escrow.get_delegate(&delegator), None);

    // Former delegate can no longer cast a delegated vote on delegator's behalf
    ctx.escrow.cast_vote(&delegate, &id, &VoteSupport::For);
    assert_eq!(
        ctx.escrow
            .try_cast_delegated_vote(&delegate, &id, &delegator),
        Err(Ok(GovError::NotDelegate))
    );

    // Delegator can now vote directly with their full weight
    ctx.escrow.cast_vote(&delegator, &id, &VoteSupport::Against);
    let r = ctx.escrow.get_vote_receipt(&id, &delegator).unwrap();
    assert_eq!(r.weight, 90);
    assert!(!r.via_delegate);
    assert_eq!(r.support, VoteSupport::Against);

    let p = ctx.escrow.get_governance_proposal(&id).unwrap();
    assert_eq!(p.for_weight, 40);
    assert_eq!(p.against_weight, 90);
}

#[test]
fn self_delegation_clears_existing_delegation() {
    let ctx = setup();
    let delegate = voter_with(&ctx, 40, 500_000);
    let delegator = voter_with(&ctx, 90, 500_000);

    // Set delegation to delegate
    ctx.escrow.delegate(&delegator, &delegate);
    assert_eq!(ctx.escrow.get_delegate(&delegator), Some(delegate.clone()));

    // Self-delegation (delegating to oneself) clears the delegation
    ctx.escrow.delegate(&delegator, &delegator);
    assert_eq!(ctx.escrow.get_delegate(&delegator), None);

    // Verify direct voting is restored
    let id = ctx
        .escrow
        .propose_governance(&delegate, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&delegator, &id, &VoteSupport::For);
    let r = ctx.escrow.get_vote_receipt(&id, &delegator).unwrap();
    assert_eq!(r.weight, 90);
    assert!(!r.via_delegate);
}

#[test]
fn undelegate_when_not_delegated_succeeds_gracefully() {
    let ctx = setup();
    let delegator = voter_with(&ctx, 90, 500_000);

    assert_eq!(ctx.escrow.get_delegate(&delegator), None);
    // Undelegating when no delegation is active does not error
    ctx.escrow.undelegate(&delegator);
    assert_eq!(ctx.escrow.get_delegate(&delegator), None);
}

// ============================================================
// Governance / multisig separation of powers
// ============================================================

#[test]
fn multisig_cannot_change_governed_parameter_once_enabled() {
    let ctx = setup();
    // The signer tries to change the fee directly via the multisig path.
    assert_eq!(
        ctx.escrow
            .try_propose_admin_action(&ctx.signer, &AdminAction::SetFeeBps(300)),
        Err(Ok(EscrowError::GovernanceRequired))
    );
    // Treasury too.
    let t = Address::generate(&ctx.env);
    assert_eq!(
        ctx.escrow
            .try_propose_admin_action(&ctx.signer, &AdminAction::SetTreasury(t)),
        Err(Ok(EscrowError::GovernanceRequired))
    );
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

#[test]
fn multisig_retains_operational_actions() {
    let ctx = setup();
    // Unpause is operational (multisig-only) and auto-executes with threshold 1.
    ctx.escrow
        .propose_admin_action(&ctx.signer, &AdminAction::Unpause);
    // Adding a signer is likewise still a multisig power.
    let new_signer = Address::generate(&ctx.env);
    ctx.escrow
        .propose_admin_action(&ctx.signer, &AdminAction::AddSigner(new_signer));
    // Sanity: governed parameter unchanged throughout.
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

#[test]
fn governance_rejects_non_governable_action() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    assert_eq!(
        ctx.escrow
            .try_propose_governance(&a, &AdminAction::Pause),
        Err(Ok(GovError::NotGovernable))
    );
    let s = Address::generate(&ctx.env);
    assert_eq!(
        ctx.escrow
            .try_propose_governance(&a, &AdminAction::AddSigner(s)),
        Err(Ok(GovError::NotGovernable))
    );
}

#[test]
fn multisig_controls_parameters_before_governance_enabled() {
    // When governance is NOT configured, the multisig can still change fees —
    // the separation of powers only activates once governance is enabled.
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &escrow_id);
    let signer = Address::generate(&env);
    let treasury = Address::generate(&env);
    let signers = soroban_sdk::vec![&env, signer.clone()];
    escrow.initialize(&signers, &1, &treasury, &100, &604_800);

    // Threshold 1, no time-lock on SetFeeBps -> auto-executes.
    escrow.propose_admin_action(&signer, &AdminAction::SetFeeBps(300));
    assert_eq!(escrow.get_fee_bps(), 300);
}

// ============================================================
// Lifecycle: timelock, grace, expiry
// ============================================================

#[test]
fn cannot_finalize_while_voting_open() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    assert_eq!(
        ctx.escrow.try_finalize_governance(&id),
        Err(Ok(GovError::VotingActive))
    );
}

#[test]
fn cannot_vote_after_window_closes() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    advance(&ctx, VOTING_PERIOD + 1);
    assert_eq!(
        ctx.escrow.try_cast_vote(&a, &id, &VoteSupport::For),
        Err(Ok(GovError::VotingClosed))
    );
}

#[test]
fn queued_proposal_expires_after_grace_window() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);

    advance(&ctx, VOTING_PERIOD + 1);
    ctx.escrow.finalize_governance(&id);

    // Blow past eta + grace without executing.
    advance(&ctx, TIMELOCK + GRACE + 10);
    ctx.escrow.expire_governance(&id);
    assert_eq!(
        ctx.escrow.get_governance_proposal(&id).unwrap().status,
        GovProposalStatus::Expired
    );
    // Execution now fails.
    assert_eq!(
        ctx.escrow.try_execute_governance(&id),
        Err(Ok(GovError::NotQueued))
    );
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

#[test]
fn execute_after_grace_is_refused_then_expirable() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&a, &AdminAction::SetFeeBps(250));
    ctx.escrow.cast_vote(&a, &id, &VoteSupport::For);
    advance(&ctx, VOTING_PERIOD + 1);
    ctx.escrow.finalize_governance(&id);
    advance(&ctx, TIMELOCK + GRACE + 10);

    // Execution past the grace window is refused (and, being an Err, does not
    // mutate state — the proposal is still Queued until formally expired).
    assert_eq!(
        ctx.escrow.try_execute_governance(&id),
        Err(Ok(GovError::Expired))
    );
    assert_eq!(
        ctx.escrow.get_governance_proposal(&id).unwrap().status,
        GovProposalStatus::Queued
    );

    // The dedicated cleanup call persists the terminal Expired status.
    ctx.escrow.expire_governance(&id);
    assert_eq!(
        ctx.escrow.get_governance_proposal(&id).unwrap().status,
        GovProposalStatus::Expired
    );
    assert_eq!(ctx.escrow.get_fee_bps(), 100);
}

// ============================================================
// Config / validation
// ============================================================

#[test]
fn get_governance_config_returns_configured_values() {
    let ctx = setup();

    let config = ctx.escrow.get_governance_config().unwrap();
    assert_eq!(config.reputation, ctx.rep_id);
    assert_eq!(config.voting_period_secs, VOTING_PERIOD);
    assert_eq!(config.timelock_secs, TIMELOCK);
    assert_eq!(config.grace_secs, GRACE);
    assert_eq!(config.quorum_votes, QUORUM);
    assert_eq!(config.pass_threshold_bps, PASS_BPS);
    assert_eq!(config.min_proposer_weight, 0);
}

#[test]
fn propose_requires_governance_configured() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &escrow_id);
    let signer = Address::generate(&env);
    let treasury = Address::generate(&env);
    let signers = soroban_sdk::vec![&env, signer.clone()];
    escrow.initialize(&signers, &1, &treasury, &100, &604_800);

    let a = Address::generate(&env);
    assert_eq!(
        escrow.try_propose_governance(&a, &AdminAction::SetFeeBps(250)),
        Err(Ok(GovError::NotConfigured))
    );
}

#[test]
fn configure_governance_requires_signer() {
    let ctx = setup();
    let stranger = Address::generate(&ctx.env);
    assert_eq!(
        ctx.escrow.try_configure_governance(
            &stranger,
            &ctx.rep_id,
            &VOTING_PERIOD,
            &TIMELOCK,
            &GRACE,
            &QUORUM,
            &PASS_BPS,
            &0,
        ),
        Err(Ok(GovError::NotAdmin))
    );
}

#[test]
fn configure_governance_rejects_invalid_params() {
    let ctx = setup();
    // pass_threshold_bps out of range.
    assert_eq!(
        ctx.escrow.try_configure_governance(
            &ctx.signer,
            &ctx.rep_id,
            &VOTING_PERIOD,
            &TIMELOCK,
            &GRACE,
            &QUORUM,
            &10_001,
            &0,
        ),
        Err(Ok(GovError::InvalidParam))
    );
    // zero voting period.
    assert_eq!(
        ctx.escrow.try_configure_governance(
            &ctx.signer,
            &ctx.rep_id,
            &0,
            &TIMELOCK,
            &GRACE,
            &QUORUM,
            &PASS_BPS,
            &0,
        ),
        Err(Ok(GovError::InvalidParam))
    );
}

#[test]
fn propose_rejects_fee_above_max() {
    let ctx = setup();
    let a = voter_with(&ctx, 200, 500_000);
    // MAX_FEE_BPS is 500; 600 is out of range.
    assert_eq!(
        ctx.escrow
            .try_propose_governance(&a, &AdminAction::SetFeeBps(600)),
        Err(Ok(GovError::InvalidFee))
    );
}

#[test]
fn min_proposer_weight_is_enforced() {
    let ctx = setup();
    // Re-tune governance to require a proposer weight of 100.
    ctx.escrow.configure_governance(
        &ctx.signer,
        &ctx.rep_id,
        &VOTING_PERIOD,
        &TIMELOCK,
        &GRACE,
        &QUORUM,
        &PASS_BPS,
        &100,
    );
    let weak = voter_with(&ctx, 50, 500_000);
    assert_eq!(
        ctx.escrow
            .try_propose_governance(&weak, &AdminAction::SetFeeBps(250)),
        Err(Ok(GovError::NoVotingPower))
    );
    // A sufficiently-weighty proposer succeeds.
    let strong = voter_with(&ctx, 150, 500_000);
    let id = ctx
        .escrow
        .propose_governance(&strong, &AdminAction::SetFeeBps(250));
    assert_eq!(id, 1);
}
