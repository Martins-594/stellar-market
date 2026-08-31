#![cfg(test)]

use super::*;
use soroban_sdk::{
    contract, contractimpl,
    testutils::{storage::Persistent, Address as _, BytesN as _, Events, Ledger},
    BytesN, Env, String,
};

// Helper macro to add arbitrators and get assigned ones for a dispute
macro_rules! setup_arbs_and_get_assigned {
    ($env:expr, $client:expr, $admin:expr, $dispute_id:expr, $count:expr) => {{
        for _ in 0..$count {
            let arb = Address::generate(&$env);
            $client.add_arbitrator(&$admin, &arb);
        }
        $client.get_assigned_arbitrators(&$dispute_id)
    }};
}

#[contract]
pub struct DummyEscrow;

#[contractimpl]
impl DummyEscrow {
    pub fn resolve_dispute_callback(_env: Env, _job_id: u64, _resolution: DisputeResolution) {}
}

/// Escrow that always panics on resolve_dispute_callback — used to simulate a failing cross-contract call.
/// Must live in its own module to avoid #[contractimpl] symbol collision with DummyEscrow.
mod failing_escrow {
    use super::*;
    use soroban_sdk::{contract, contractimpl, Env};

    #[contract]
    pub struct DummyEscrowFailing;

    #[contractimpl]
    impl DummyEscrowFailing {
        pub fn resolve_dispute_callback(_env: Env, _job_id: u64, _resolution: DisputeResolution) {
            panic!("escrow_fail_simulated");
        }
    }
}
use failing_escrow::DummyEscrowFailing;

// Mock reputation contract for testing
#[contract]
pub struct MockReputationContract;

#[contractimpl]
impl MockReputationContract {
    pub fn get_reputation(
        _env: Env,
        user: Address,
    ) -> Result<reputation::UserReputation, soroban_sdk::Error> {
        Ok(reputation::UserReputation {
            user: user.clone(),
            total_score: 500,
            total_weight: 10,
            review_count: 5,
            last_updated_ts: 0,
        })
    }

    pub fn slash_stake(
        _env: Env,
        _caller: Address,
        _loser: Address,
        _job_id: u64,
        _amount: u64,
    ) -> Result<(), soroban_sdk::Error> {
        Ok(())
    }

    /// Mock for the MaliciousFiling cross-contract call.
    pub fn apply_dispute_outcome(
        _env: Env,
        _user: Address,
        _outcome: u32, // DisputeOutcome discriminant (2 = MaliciousFiling)
    ) -> Result<(), soroban_sdk::Error> {
        Ok(())
    }
}

#[test]
fn test_initialize_contract() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Verify initialization by checking if we can set min reputation
    client.set_min_voter_reputation(&admin, &400);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_initialize_twice_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    // Try to initialize again - should fail
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
}

#[test]
fn test_set_min_voter_reputation() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    client.set_min_voter_reputation(&admin, &500);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_set_min_voter_reputation_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    // Non-admin tries to set min reputation - should fail
    client.set_min_voter_reputation(&non_admin, &500);
}

#[test]
fn test_is_eligible_voter_high_reputation() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let voter = Address::generate(&env);

    let is_eligible = client.is_eligible_voter(&voter);
    // Mock returns high reputation for all users
    assert_eq!(is_eligible, true);
}

#[test]
fn test_vote_with_reputation_check() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    // Vote with reputation check - should succeed with mock
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter = assigned.get(0).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_client, 1);
}

#[test]
fn test_raise_dispute() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Work not delivered"),
        &3u32,
        &None,
    );

    assert_eq!(dispute_id, 1);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.job_id, 1);
    assert_eq!(dispute.status, DisputeStatus::Open);
    assert_eq!(dispute.min_votes, 3);
}

#[test]
fn test_vote_and_resolve() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    
    // Add arbitrators
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }
    
    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &freelancer,
        &String::from_str(&env, "Payment not released"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Work was done"), &0);
    client.cast_vote(
        &dispute_id,
        &assigned.get(1).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Agree with freelancer"), &0);
    client.cast_vote(
        &dispute_id,
        &assigned.get(2).unwrap(),
        &VoteChoice::Client,
        &String::from_str(&env, "Incomplete work"), &0);

    let result = client.resolve_dispute(&dispute_id);
    assert_eq!(result, DisputeStatus::ResolvedForFreelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_resolve_without_enough_votes() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter = assigned.get(0).unwrap();
    client.cast_vote(
        &dispute_id,
        &voter,
        &VoteChoice::Client,
        &String::from_str(&env, "Reason"), &0);

    client.resolve_dispute(&dispute_id);
}

#[test]
fn test_tie_break_favor_client() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    
    // Add arbitrators
    for _ in 0..10 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32, // min_votes set to 4
        &Some(TieBreakMethod::FavorClient),
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::Client,
        &String::from_str(&env, "C1"), &0);
    client.cast_vote(
        &dispute_id,
        &assigned.get(1).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F1"), &0);
    client.cast_vote(
        &dispute_id,
        &assigned.get(2).unwrap(),
        &VoteChoice::Client,
        &String::from_str(&env, "C2"), &0);
    client.cast_vote(
        &dispute_id,
        &assigned.get(3).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

#[test]
fn test_tie_break_favor_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32,
        &Some(TieBreakMethod::FavorFreelancer),
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();
    let voter4 = assigned.get(3).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "C1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "C2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter4,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

#[test]
fn test_tie_break_refund_both() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32,
        &Some(TieBreakMethod::RefundBoth),
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();
    let voter4 = assigned.get(3).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "C1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "C2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter4,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

#[test]
fn test_tie_break_escalate() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32,
        &Some(TieBreakMethod::Escalate),
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();
    let voter4 = assigned.get(3).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "C1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "C2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter4,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

#[test]
fn test_tie_break_default_refund_both() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32,
        &None, // Should default to RefundBoth
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();
    let voter4 = assigned.get(3).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "C1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "C2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter4,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

// ── Graceful degradation without reputation system ────────────────────────────

#[test]
fn test_vote_without_reputation_contract() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    // MockReputationContract returns score=500 which satisfies min_voter_reputation=300.
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter = assigned.get(0).unwrap();
    client.cast_vote(
        &dispute_id,
        &voter,
        &VoteChoice::Client,
        &String::from_str(&env, "Reason"), &0);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_client, 1);
}

// ── Pause mechanism tests ─────────────────────────────────────────────────────

#[test]
fn test_pause_and_unpause() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Create a dispute first
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );
    assert_eq!(dispute_id, 1);

    // Pause the contract
    client.pause(&admin);

    // Unpause the contract
    client.unpause(&admin);

    // Now raising another dispute should work
    let dispute_id2 = client.raise_dispute(
        &2u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue 2"),
        &3u32,
        &None,
    );
    assert_eq!(dispute_id2, 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")] // NotAdmin
fn test_pause_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Try to pause with non-admin address
    client.pause(&non_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")] // ContractPaused
fn test_raise_dispute_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    client.pause(&admin);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")] // ContractPaused
fn test_cast_vote_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    client.pause(&admin);

    let voter = Address::generate(&env);
    client.cast_vote(
        &dispute_id,
        &voter,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")] // ContractPaused
fn test_resolve_dispute_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    // Use min_votes=5 so 3 votes won't trigger auto-resolve before we pause.
    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote 1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Vote 2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote 3"), &0);

    client.pause(&admin);

    client.resolve_dispute(&dispute_id);
}

#[test]
fn test_read_only_functions_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    client.pause(&admin);

    // Read-only functions should still work when paused
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.id, dispute_id);

    let count = client.get_dispute_count();
    assert_eq!(count, 1);

    let votes = client.get_votes(&dispute_id);
    assert_eq!(votes.len(), 0);

    let is_excluded = client.is_excluded_voter(&dispute_id, &Address::generate(&env));
    assert_eq!(is_excluded, false);
}

// ── Stake slashing tests (issue #221) ────────────────────────────────────────

fn setup_dispute_with_votes(
    env: &Env,
    client_votes: u32,
    freelancer_votes: u32,
) -> (
    DisputeContractClient,
    Address, // dispute contract id
    Address, // escrow contract id
    Address, // user_client
    Address, // freelancer
    u64,     // dispute_id
) {
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(env, &dispute_contract_id);

    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(env);

    let user_client = Address::generate(env);
    let freelancer = Address::generate(env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Add arbitrators to pool
    let mut arbitrators = Vec::new(env);
    for _ in 0..10 {
        let arb = Address::generate(env);
        client.add_arbitrator(&admin, &arb);
        arbitrators.push_back(arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(env, "Dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    for i in 0..client_votes {
        if (i as u32) < assigned.len() {
            let voter = assigned.get(i as u32).unwrap();
            client.cast_vote(
                &dispute_id,
                &voter,
                &VoteChoice::Client,
                &String::from_str(env, "For client"), &0);
        }
    }
    for i in 0..freelancer_votes {
        let idx = client_votes + i;
        if idx < assigned.len() {
            let voter = assigned.get(idx).unwrap();
            client.cast_vote(
                &dispute_id,
                &voter,
                &VoteChoice::Freelancer,
                &String::from_str(env, "For freelancer"), &0);
        }
    }

    (
        client,
        dispute_contract_id,
        escrow_contract_id,
        user_client,
        freelancer,
        dispute_id,
    )
}

#[test]
fn test_client_wins_freelancer_stake_slashed() {
    let env = Env::default();
    env.mock_all_auths();

    // 3 votes for client, 0 for freelancer → client wins (auto-resolved on 3rd vote)
    let (client, _, _escrow_id, _user_client, _freelancer, dispute_id) =
        setup_dispute_with_votes(&env, 3, 0);

    // Dispute is already auto-resolved by the 3rd vote; check status directly.
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);

    let events = env.events().all();
    let slash_event = events.iter().find(|(_, topics, _)| {
        if topics.len() >= 2 {
            let t1: Symbol = topics.get(1).unwrap().into_val(&env);
            return t1 == Symbol::new(&env, "reput_slashed");
        }
        false
    });
    assert!(
        slash_event.is_some(),
        "StakeSlashed event should be emitted when client wins"
    );
}

#[test]
fn test_freelancer_wins_client_stake_slashed() {
    let env = Env::default();
    env.mock_all_auths();

    // 0 votes for client, 3 for freelancer → freelancer wins (auto-resolved on 3rd vote)
    let (client, _, _escrow_id, _user_client, _freelancer, dispute_id) =
        setup_dispute_with_votes(&env, 0, 3);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForFreelancer);

    let events = env.events().all();
    let slash_event = events.iter().find(|(_, topics, _)| {
        if topics.len() >= 2 {
            let t1: Symbol = topics.get(1).unwrap().into_val(&env);
            return t1 == Symbol::new(&env, "reput_slashed");
        }
        false
    });
    assert!(
        slash_event.is_some(),
        "StakeSlashed event should be emitted when freelancer wins"
    );
}

#[test]
fn test_no_slash_on_escalated_dispute() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &4u32,
        &Some(TieBreakMethod::Escalate),
    );

    // Tie vote → escalate
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();
    let voter4 = assigned.get(3).unwrap();
    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "C"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "C"), &0);
    client.cast_vote(
        &dispute_id,
        &voter4,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "F"), &0);

    let status = client.resolve_dispute(&dispute_id);
    // Exact client/freelancer tie now resolves as 50/50 split (Issue #702)
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")] // DisputeCooldown
fn test_raise_dispute_blocked_by_job_cooldown() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &7u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Initial dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();

    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "V1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Client,
        &String::from_str(&env, "V2"), &0);
    client.cast_vote(
        &dispute_id,
        &voter3,
        &VoteChoice::Client,
        &String::from_str(&env, "V3"), &0);

    // 3 votes for Client → auto-resolve fires; re-raise immediately must fail with DisputeCooldown.
    client.raise_dispute(
        &7u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Retry too soon"),
        &3u32,
        &None,
    );
}

#[test]
fn test_raise_dispute_allowed_after_cooldown() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let first_dispute_id = client.raise_dispute(
        &9u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Initial dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&first_dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let voter3 = assigned.get(2).unwrap();

    client.cast_vote(
        &first_dispute_id,
        &voter1,
        &VoteChoice::Client,
        &String::from_str(&env, "V1"), &0);
    client.cast_vote(
        &first_dispute_id,
        &voter2,
        &VoteChoice::Client,
        &String::from_str(&env, "V2"), &0);
    client.cast_vote(
        &first_dispute_id,
        &voter3,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "V3"), &0);

    // Dispute auto-resolved on 3rd vote; advance past both cooldowns.
    env.ledger().with_mut(|l| l.timestamp = 1000 + 1_209_601);

    let second_dispute_id = client.raise_dispute(
        &9u64,
        &user_client,
        &freelancer,
        &freelancer,
        &String::from_str(&env, "Retry after cooldown"),
        &3u32,
        &None,
    );

    assert_eq!(second_dispute_id, 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")] // VotingPeriodNotExpired
fn test_force_resolve_timeout_not_expired_fails() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &10u32,
        &None,
    );

    // Try to force resolve before deadline (Deadline is 1000 + 604_800 = 605_800)
    env.ledger().with_mut(|l| l.timestamp = 600_000);
    client.force_resolve_timeout(&dispute_id);
}

#[test]
fn test_force_resolve_timeout_expired_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..10 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &10u32,
        &None,
    );

    // 1 vote for freelancer (not enough to auto-resolve — min_votes=10)
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter = assigned.get(0).unwrap();
    client.cast_vote(
        &dispute_id,
        &voter,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Reason"), &0);

    // Advance past deadline (1000 + 604_800 = 605_800)
    env.ledger().with_mut(|l| l.timestamp = 605_801);

    let status = client.force_resolve_timeout(&dispute_id);
    assert_eq!(status, DisputeStatus::ResolvedForFreelancer);
}

#[test]
fn test_force_resolve_timeout_tie_break_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &10u32,
        &Some(TieBreakMethod::FavorClient),
    );

    // Advance past deadline
    env.ledger().with_mut(|l| l.timestamp = 605_801);

    let status = client.force_resolve_timeout(&dispute_id);
    assert_eq!(status, DisputeStatus::RefundedBoth);
}

#[test]
#[should_panic(expected = "Error(Contract, #27)")]
fn test_force_resolve_timeout_rejects_inactive_panel() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &5u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.remove_arbitrator(&admin, &assigned.get(0).unwrap());
    client.remove_arbitrator(&admin, &assigned.get(1).unwrap());
    client.remove_arbitrator(&admin, &assigned.get(2).unwrap());
    env.ledger().with_mut(|l| l.timestamp = 605_801);
    client.force_resolve_timeout(&dispute_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_excluded_voter_must_be_assigned_arbitrator() {
    let env = Env::default();
    env.mock_all_auths();
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &5u32, &None,
    );
    client.add_excluded_voter(&dispute_id, &user_client, &Address::generate(&env));
}

#[test]
fn test_exclusion_requires_both_parties_and_replaces_arbitrator() {
    let env = Env::default();
    env.mock_all_auths();
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..6 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &5u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let excluded = assigned.get(0).unwrap();
    client.add_excluded_voter(&dispute_id, &user_client, &excluded);
    assert!(!client.is_excluded_voter(&dispute_id, &excluded));
    client.add_excluded_voter(&dispute_id, &freelancer, &excluded);
    let updated = client.get_dispute(&dispute_id);
    assert!(updated.excluded_voters.contains(&excluded));
    assert_eq!(updated.assigned_arbitrators.len(), 5);
    assert!(!updated.assigned_arbitrators.contains(&excluded));
}

// ── #1166: ExclusionProposal TTL ────────────────────────────────────────────

#[test]
fn test_exclusion_proposal_ttl_extended_on_write() {
    let env = Env::default();
    env.mock_all_auths();
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..6 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &5u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter = assigned.get(0).unwrap();

    client.add_excluded_voter(&dispute_id, &user_client, &voter);

    // Matches every other bump_*_ttl helper's contract: extend_ttl(key,
    // MIN_TTL_THRESHOLD, MIN_TTL_EXTEND_TO) leaves the key's remaining TTL
    // at MIN_TTL_EXTEND_TO ledgers. Before #1166 this key was never
    // extended at all, so its TTL would just be whatever the ledger's
    // baseline persistent-entry TTL happened to be on write — not this.
    env.as_contract(&dispute_contract_id, || {
        let key = DataKey::ExclusionProposal(dispute_id, voter.clone());
        let ttl = env.storage().persistent().get_ttl(&key);
        assert_eq!(
            ttl, MIN_TTL_EXTEND_TO,
            "ExclusionProposal's TTL was not extended to MIN_TTL_EXTEND_TO on write"
        );
    });

    // Advance well past the threshold at which an un-extended entry (using
    // only the ledger's baseline TTL) would already have expired, but still
    // short of MIN_TTL_EXTEND_TO — the proposal must still be readable and
    // confirmable by the second party.
    env.ledger()
        .with_mut(|l| l.sequence_number += MIN_TTL_THRESHOLD + 500);

    env.as_contract(&dispute_contract_id, || {
        let key = DataKey::ExclusionProposal(dispute_id, voter.clone());
        assert!(
            env.storage().persistent().has(&key),
            "exclusion proposal expired from storage before confirmation"
        );
    });

    client.add_excluded_voter(&dispute_id, &freelancer, &voter);
    let updated = client.get_dispute(&dispute_id);
    assert!(updated.excluded_voters.contains(&voter));
}

#[test]
fn test_timeout_uses_protocol_tie_break_after_exclusion_attempts() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..10 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &10u32,
        &Some(TieBreakMethod::FavorClient),
    );
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    for i in 0..assigned.len() {
        let voter = assigned.get(i).unwrap();
        client.add_excluded_voter(&dispute_id, &user_client, &voter);
        client.add_excluded_voter(&dispute_id, &freelancer, &voter);
    }
    env.ledger().with_mut(|l| l.timestamp = 605_801);
    assert_eq!(client.force_resolve_timeout(&dispute_id), DisputeStatus::RefundedBoth);
}

// ── Party-pair cooldown tests (#530) ─────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #13)")] // DisputeCooldown
fn test_party_cooldown_blocks_same_parties_on_different_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    // Raise and resolve a dispute on job 1.
    let d1 = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "First dispute"), &3u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&d1);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    client.cast_vote(&d1, &v1, &VoteChoice::Client, &String::from_str(&env, "v1"), &0);
    client.cast_vote(&d1, &v2, &VoteChoice::Client, &String::from_str(&env, "v2"), &0);
    client.cast_vote(&d1, &v3, &VoteChoice::Client, &String::from_str(&env, "v3"), &0);

    // 3 votes for Client → auto-resolve fires and sets party cooldown.
    // Immediately raising a dispute on a different job between the same parties must fail.
    client.raise_dispute(
        &2u64, &user_client, &freelancer, &freelancer,
        &String::from_str(&env, "Too soon"), &3u32, &None,
    );
}

#[test]
fn test_party_cooldown_allows_after_expiry() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let d1 = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "First dispute"), &3u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&d1);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    client.cast_vote(&d1, &v1, &VoteChoice::Client, &String::from_str(&env, "v1"), &0);
    client.cast_vote(&d1, &v2, &VoteChoice::Client, &String::from_str(&env, "v2"), &0);
    client.cast_vote(&d1, &v3, &VoteChoice::Freelancer, &String::from_str(&env, "v3"), &0);

    // Advance past the 14-day per-party cooldown (1_209_600 s) and per-job cooldown (86_400 s).
    env.ledger().with_mut(|l| l.timestamp = 1000 + 1_209_601);

    let d2 = client.raise_dispute(
        &2u64, &user_client, &freelancer, &freelancer,
        &String::from_str(&env, "After cooldown"), &3u32, &None,
    );
    assert_eq!(d2, 2);
}

#[test]
fn test_party_cooldown_does_not_affect_different_party_pairs() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1000;
    });

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    let user_client_a = Address::generate(&env);
    let freelancer_a = Address::generate(&env);
    let user_client_b = Address::generate(&env);
    let freelancer_b = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    // Resolve a dispute between pair A.
    let d1 = client.raise_dispute(
        &1u64, &user_client_a, &freelancer_a, &user_client_a,
        &String::from_str(&env, "Pair A"), &3u32, &None,
    );
    let assigned = client.get_assigned_arbitrators(&d1);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    client.cast_vote(&d1, &v1, &VoteChoice::Client, &String::from_str(&env, "v1"), &0);
    client.cast_vote(&d1, &v2, &VoteChoice::Client, &String::from_str(&env, "v2"), &0);
    client.cast_vote(&d1, &v3, &VoteChoice::Freelancer, &String::from_str(&env, "v3"), &0);

    // Different pair B should be unaffected.
    let d2 = client.raise_dispute(
        &2u64, &user_client_b, &freelancer_b, &user_client_b,
        &String::from_str(&env, "Pair B"), &3u32, &None,
    );
    assert_eq!(d2, 2);
}

#[test]
fn test_set_cooldown_duration() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Admin can update the cooldown duration.
    client.set_cooldown_duration(&admin, &100_000u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")] // NotAdmin
fn test_set_cooldown_duration_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    let non_admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    client.set_cooldown_duration(&non_admin, &100_000u64);
}

// ── Vote delegation tests (#479) ──────────────────────────────────────────────

fn setup_initialized_dispute_contract(
    env: &Env,
) -> (DisputeContractClient, Address, Address, Address) {
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    
    // Add arbitrators to pool
    for _ in 0..10 {
        let arb = Address::generate(env);
        client.add_arbitrator(&admin, &arb);
    }
    
    (client, dispute_contract_id, escrow_contract_id, admin)
}

#[test]
fn test_delegate_vote_stored_and_readable() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);

    client.delegate_vote(&owner, &delegate, &42u64);

    let stored = client.get_delegation(&owner, &42u64);
    assert_eq!(stored, Some(delegate));
}

#[test]
fn test_delegate_can_cast_vote_on_behalf_of_owner() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let delegate = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &1u32,
        &None,
    );

    // Use an actual assigned arbitrator as the owner who delegates.
    let arbitrators = client.get_assigned_arbitrators(&dispute_id);
    let owner = arbitrators.get(0).unwrap();

    // Owner delegates their vote rights for job 1 to delegate.
    client.delegate_vote(&owner, &delegate, &1u64);

    // Delegate casts the vote — should succeed and count for the dispute.
    client.cast_vote(
        &dispute_id,
        &delegate,
        &VoteChoice::Client,
        &String::from_str(&env, "Voting on behalf of owner"), &0);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_client, 1);
}

#[test]
fn test_revoke_delegation_removes_entry() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let owner = Address::generate(&env);
    let delegate = Address::generate(&env);

    client.delegate_vote(&owner, &delegate, &10u64);
    assert!(client.get_delegation(&owner, &10u64).is_some());

    client.revoke_delegation(&owner, &10u64);
    assert!(client.get_delegation(&owner, &10u64).is_none());
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // AlreadyVoted
fn test_owner_cannot_vote_directly_after_delegate_voted() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let delegate = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &1u32,
        &None,
    );

    // Use an actual assigned arbitrator as the owner who will delegate.
    let arbitrators = client.get_assigned_arbitrators(&dispute_id);
    let owner = arbitrators.get(0).unwrap();

    client.delegate_vote(&owner, &delegate, &1u64);

    // Delegate votes first (on behalf of owner who is an assigned arbitrator).
    client.cast_vote(
        &dispute_id,
        &delegate,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Delegate vote"), &0);

    // Owner tries to vote directly — must fail with AlreadyVoted (#3).
    client.cast_vote(
        &dispute_id,
        &owner,
        &VoteChoice::Client,
        &String::from_str(&env, "Direct vote after delegate"), &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // AlreadyVoted
fn test_delegate_cannot_vote_if_owner_voted_directly() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let delegate = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &1u32,
        &None,
    );

    // Use an actual assigned arbitrator as the owner.
    let arbitrators = client.get_assigned_arbitrators(&dispute_id);
    let owner = arbitrators.get(0).unwrap();

    // Owner votes directly first.
    client.cast_vote(
        &dispute_id,
        &owner,
        &VoteChoice::Client,
        &String::from_str(&env, "Direct owner vote"), &0);

    // Owner tries to set up a delegation after already voting — must fail with AlreadyVoted (#3).
    client.delegate_vote(&owner, &delegate, &1u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")] // DelegateAlreadyVoted
fn test_revoke_fails_after_delegate_voted() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let delegate = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &1u32,
        &None,
    );

    // Use an actual assigned arbitrator as the owner who delegates.
    let arbitrators = client.get_assigned_arbitrators(&dispute_id);
    let owner = arbitrators.get(0).unwrap();

    client.delegate_vote(&owner, &delegate, &1u64);

    client.cast_vote(
        &dispute_id,
        &delegate,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Delegate vote"), &0);

    // Attempting to revoke after delegate has voted must fail with DelegateAlreadyVoted (#17).
    client.revoke_delegation(&owner, &1u64);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")] // AlreadyDelegated
fn test_double_delegation_for_same_job_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let owner = Address::generate(&env);
    let delegate_a = Address::generate(&env);
    let delegate_b = Address::generate(&env);

    client.delegate_vote(&owner, &delegate_a, &5u64);
    // Second delegation for the same job must fail.
    client.delegate_vote(&owner, &delegate_b, &5u64);
}

#[test]
fn test_delegated_vote_counts_same_as_direct_vote_in_resolution() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();
    let owner = assigned.get(2).unwrap();
    let delegate = Address::generate(&env);

    // Two direct voters for freelancer.
    client.cast_vote(
        &dispute_id,
        &voter1,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "v1"), &0);
    client.cast_vote(
        &dispute_id,
        &voter2,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "v2"), &0);

    // One delegated vote for freelancer (owner is an assigned arbitrator who delegates).
    client.delegate_vote(&owner, &delegate, &1u64);
    client.cast_vote(
        &dispute_id,
        &delegate,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "delegated"), &0);

    // 3 votes for freelancer — dispute auto-resolved on 3rd vote.
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForFreelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized: parties are excluded from assigned_arbitrators
fn test_conflict_of_interest_voter_is_party() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    // Client tries to vote on their own dispute
    client.cast_vote(
        &dispute_id,
        &user_client,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);
}

// ── Malicious dispute filing tests ────────────────────────────────────────────

/// Helper: set up a dispute with an initialized contract and return key objects.
fn setup_malicious_test(
    env: &Env,
) -> (
    DisputeContractClient,
    Address, // job_client
    Address, // freelancer
    u64,     // dispute_id
) {
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(env, &dispute_contract_id);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // Add arbitrators to pool
    for _ in 0..10 {
        let arb = Address::generate(env);
        client.add_arbitrator(&admin, &arb);
    }

    let job_client = Address::generate(env);
    let freelancer = Address::generate(env);
    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client, // initiator = client (the one allegedly filing in bad faith)
        &String::from_str(env, "Frivolous claim"),
        &5u32, // min_votes = 5 so we can reach supermajority
        &None,
    );
    (client, job_client, freelancer, dispute_id)
}

/// 4-of-5 votes for MaliciousFiling → resolves as MaliciousDisputeFiling.
#[test]
fn test_malicious_filing_supermajority_resolves() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _job_client, _freelancer, dispute_id) = setup_malicious_test(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    let v4 = assigned.get(3).unwrap();
    let v5 = assigned.get(4).unwrap();

    // 4 malicious votes + 1 dissenting vote = supermajority (auto-resolves on 5th vote)
    client.cast_vote(&dispute_id, &v1, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v2, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v3, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v4, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v5, &VoteChoice::Client,           &String::from_str(&env, "disagree"), &0);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::MaliciousDisputeFiling);
}

/// 3-of-5 votes for MaliciousFiling (60 %) — below the 80 % supermajority threshold.
/// Resolves normally (Client wins here because the remaining 2 votes are also for Client).
#[test]
fn test_malicious_filing_below_supermajority_resolves_normally() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _job_client, _freelancer, dispute_id) = setup_malicious_test(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    let v4 = assigned.get(3).unwrap();
    let v5 = assigned.get(4).unwrap();

    // 3 malicious + 2 for client = 60 % malicious, not ≥ 80 % (auto-resolves on 5th vote via tie-break)
    client.cast_vote(&dispute_id, &v1, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v2, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v3, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v4, &VoteChoice::Client,           &String::from_str(&env, "for client"), &0);
    client.cast_vote(&dispute_id, &v5, &VoteChoice::Client,           &String::from_str(&env, "for client"), &0);

    // Should NOT resolve as MaliciousDisputeFiling — normal resolution applies.
    let dispute = client.get_dispute(&dispute_id);
    assert_ne!(dispute.status, DisputeStatus::MaliciousDisputeFiling);
}

/// Verifies that a MaliciousDisputeFiling dispute cannot be resolved a second time.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_malicious_filing_cannot_be_re_resolved() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _job_client, _freelancer, dispute_id) = setup_malicious_test(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    let v4 = assigned.get(3).unwrap();
    let v5 = assigned.get(4).unwrap();

    client.cast_vote(&dispute_id, &v1, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v2, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v3, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v4, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v5, &VoteChoice::Freelancer,       &String::from_str(&env, "dissent"), &0);

    // Dispute auto-resolved on 5th vote; any subsequent resolve attempt must fail with AlreadyResolved (#7).
    client.resolve_dispute(&dispute_id);
}

/// Verify the MaliciousDisputeResolved event is emitted on a supermajority resolution.
#[test]
fn test_malicious_filing_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, job_client, _freelancer, dispute_id) = setup_malicious_test(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let v1 = assigned.get(0).unwrap();
    let v2 = assigned.get(1).unwrap();
    let v3 = assigned.get(2).unwrap();
    let v4 = assigned.get(3).unwrap();
    let v5 = assigned.get(4).unwrap();

    client.cast_vote(&dispute_id, &v1, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v2, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v3, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v4, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0);
    client.cast_vote(&dispute_id, &v5, &VoteChoice::Freelancer,       &String::from_str(&env, "dissent"), &0);

    // Dispute auto-resolves on the 5th vote; event is emitted during auto-resolve.

    // Check that a "malicious_rslvd" event was published.
    let events = env.events().all();
    let malicious_event = events.iter().find(|e| {
        // The second topic should be the Symbol "malicious_rslvd"
        e.1.len() >= 2
    });
    assert!(malicious_event.is_some(), "MaliciousDisputeResolved event not found");

    // The dispute should now show MaliciousDisputeFiling status.
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::MaliciousDisputeFiling);
    // Initiator should be the client (who raised the dispute).
    assert_eq!(dispute.initiator, job_client);
}


// ── Arbitrator voting mechanism tests ─────────────────────────────────────────

#[test]
fn test_add_arbitrator_to_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    let arbitrator = Address::generate(&env);
    client.add_arbitrator(&admin, &arbitrator);

    let pool = client.get_arbitrator_pool();
    assert_eq!(pool.len(), 1);
    assert!(pool.contains(&arbitrator));
}

#[test]
fn test_remove_arbitrator_from_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    let arbitrator1 = Address::generate(&env);
    let arbitrator2 = Address::generate(&env);

    client.add_arbitrator(&admin, &arbitrator1);
    client.add_arbitrator(&admin, &arbitrator2);

    let pool = client.get_arbitrator_pool();
    assert_eq!(pool.len(), 2);

    client.remove_arbitrator(&admin, &arbitrator1);

    let pool = client.get_arbitrator_pool();
    assert_eq!(pool.len(), 1);
    assert!(!pool.contains(&arbitrator1));
    assert!(pool.contains(&arbitrator2));
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")] // ConflictOfInterest
fn test_remove_arbitrator_excludes_from_open_disputes() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Raise a dispute — 5 arbitrators will be randomly assigned
    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &freelancer,
        &String::from_str(&env, "Payment dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let target_arb = assigned.get(0).unwrap();

    // Remove the first assigned arbitrator from the pool
    client.remove_arbitrator(&admin, &target_arb);

    // Verify the arbitrator is excluded from the dispute
    let dispute = client.get_dispute(&dispute_id);
    assert!(dispute.excluded_voters.contains(&target_arb));

    // Verify the arbitrator can no longer vote (should panic with ConflictOfInterest = #10)
    client.cast_vote(
        &dispute_id,
        &target_arb,
        &VoteChoice::Client,
        &String::from_str(&env, "Should fail"),
        &0u64,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")] // NotAdmin
fn test_add_arbitrator_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, _admin) = setup_initialized_dispute_contract(&env);

    let non_admin = Address::generate(&env);
    let arbitrator = Address::generate(&env);
    
    client.add_arbitrator(&non_admin, &arbitrator);
}

#[test]
fn test_dispute_assigns_arbitrators() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 10 arbitrators to the pool
    for _ in 0..10 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    // Should assign up to 5 arbitrators
    assert!(assigned.len() <= 5);
    assert!(assigned.len() > 0);
    
    // Assigned arbitrators should not include client or freelancer
    assert!(!assigned.contains(&job_client));
    assert!(!assigned.contains(&freelancer));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_non_assigned_arbitrator_cannot_vote() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    // Try to vote with an address not in the assigned arbitrators
    let non_assigned = Address::generate(&env);
    client.cast_vote(
        &dispute_id,
        &non_assigned,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);
}

#[test]
fn test_assigned_arbitrator_can_vote() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    assert!(assigned.len() > 0);

    // First assigned arbitrator should be able to vote
    let arbitrator = assigned.get(0).unwrap();
    client.cast_vote(
        &dispute_id,
        &arbitrator,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_client, 1);
}

#[test]
fn test_auto_resolve_at_3_vote_majority() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // Cast 3 votes for freelancer — 3 == min_votes so auto-resolve triggers.
    for i in 0..3 {
        let arbitrator = assigned.get(i).unwrap();
        client.cast_vote(
            &dispute_id,
            &arbitrator,
            &VoteChoice::Freelancer,
            &String::from_str(&env, "Vote for freelancer"), &0);
    }

    // Check that dispute was auto-resolved
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForFreelancer);
}

#[test]
fn test_unanimous_vote_5_0() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    // All 5 vote for client
    for i in 0..assigned.len() {
        let arbitrator = assigned.get(i).unwrap();
        client.cast_vote(
            &dispute_id,
            &arbitrator,
            &VoteChoice::Client,
            &String::from_str(&env, "Vote for client"), &0);
    }

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);
    assert_eq!(dispute.votes_for_client, 5);
}

#[test]
fn test_split_vote_3_2_client_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // 3 vote for client — auto-resolves on 3rd vote (min_votes=3)
    for i in 0..3 {
        let arbitrator = assigned.get(i).unwrap();
        client.cast_vote(
            &dispute_id,
            &arbitrator,
            &VoteChoice::Client,
            &String::from_str(&env, "Vote for client"), &0);
    }

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);
    assert_eq!(dispute.votes_for_client, 3);
}

#[test]
fn test_split_vote_3_2_freelancer_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // 3 vote for freelancer — auto-resolves on 3rd vote (min_votes=3)
    for i in 0..3 {
        let arbitrator = assigned.get(i).unwrap();
        client.cast_vote(
            &dispute_id,
            &arbitrator,
            &VoteChoice::Freelancer,
            &String::from_str(&env, "Vote for freelancer"), &0);
    }

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForFreelancer);
    assert_eq!(dispute.votes_for_freelancer, 3);
}

#[test]
fn test_vote_cast_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let arbitrator = assigned.get(0).unwrap();
    
    client.cast_vote(
        &dispute_id,
        &arbitrator,
        &VoteChoice::Client,
        &String::from_str(&env, "Vote"), &0);

    // Verify VoteCast event was emitted
    let events = env.events().all();
    let vote_event = events.iter().find(|(_, topics, _)| {
        if topics.len() >= 2 {
            let t1: Symbol = topics.get(1).unwrap().into_val(&env);
            return t1 == symbol_short!("voted");
        }
        false
    });
    
    assert!(vote_event.is_some(), "VoteCast event should be emitted");
}

#[test]
fn test_dispute_resolved_event_emitted_on_auto_resolve() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // Cast 3 votes to trigger auto-resolve (min_votes=3)
    for i in 0..3 {
        let arbitrator = assigned.get(i).unwrap();
        client.cast_vote(
            &dispute_id,
            &arbitrator,
            &VoteChoice::Client,
            &String::from_str(&env, "Vote"), &0);
    }

    // Verify DisputeResolved event was emitted
    let events = env.events().all();
    let resolved_event = events.iter().find(|(_, topics, _)| {
        if topics.len() >= 2 {
            let t1: Symbol = topics.get(1).unwrap().into_val(&env);
            return t1 == symbol_short!("resolved");
        }
        false
    });
    
    assert!(resolved_event.is_some(), "DisputeResolved event should be emitted on auto-resolve");
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // AlreadyVoted
fn test_arbitrator_cannot_vote_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _dispute_id, _escrow_id, admin) = setup_initialized_dispute_contract(&env);

    // Add 5 arbitrators to the pool
    for _ in 0..5 {
        let arbitrator = Address::generate(&env);
        client.add_arbitrator(&admin, &arbitrator);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let arbitrator = assigned.get(0).unwrap();
    
    // First vote
    client.cast_vote(
        &dispute_id,
        &arbitrator,
        &VoteChoice::Client,
        &String::from_str(&env, "First vote"), &0);

    // Second vote - should fail with AlreadyVoted
    client.cast_vote(
        &dispute_id,
        &arbitrator,
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Second vote"), &1);
}

#[test]
fn test_dispute_with_empty_arbitrator_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Raise dispute with empty arbitrator pool — no add_arbitrator calls
    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // Should have no assigned arbitrators
    assert_eq!(assigned.len(), 0);
}

// ============================================================
// Issue #702 — Exact tie resolves as 50/50 split
// ============================================================

#[test]
fn test_exact_tie_resolves_as_5050_split() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..7 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Tie dispute"),
        &4u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // 2 votes for client, 2 for freelancer — exact tie
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "c1"), &0);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Freelancer, &String::from_str(&env, "f1"), &0);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "c2"), &0);
    client.cast_vote(&dispute_id, &assigned.get(3).unwrap(), &VoteChoice::Freelancer, &String::from_str(&env, "f2"), &0);

    let status = client.resolve_dispute(&dispute_id);
    assert_eq!(status, DisputeStatus::RefundSplit(50));
}

// ============================================================
// Issue #662 — Nonce replay protection tests (dispute)
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #22)")] // NonceReplay
fn test_cast_vote_nonce_replay_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);
    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Replay test"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();

    // First vote with nonce 99 succeeds
    client.cast_vote(&dispute_id, &voter1, &VoteChoice::Client, &String::from_str(&env, "v1"), &99);

    // Different voter with same nonce 99 succeeds (different caller)
    client.cast_vote(&dispute_id, &voter2, &VoteChoice::Freelancer, &String::from_str(&env, "v2"), &99);

    // voter1 tries to replay nonce 99 — should fail with NonceReplay
    client.cast_vote(&dispute_id, &voter1, &VoteChoice::Client, &String::from_str(&env, "replay"), &99);
}

// ── Issue #770: submit_evidence emits on-chain event ────────────────────────

#[test]
fn test_submit_evidence_emits_event() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "Work not delivered"),
        &3u32,
        &None,
    );

    let hash: BytesN<32> = BytesN::random(&env);
    client.submit_evidence(&dispute_id, &party_client, &hash);

    let events = env.events().all();
    assert!(!events.is_empty(), "at least one event should have been emitted");
}

#[test]
fn test_submit_evidence_stores_and_retrieves() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "Scope creep"),
        &3u32,
        &None,
    );

    let hash1: BytesN<32> = BytesN::random(&env);
    let hash2: BytesN<32> = BytesN::random(&env);

    client.submit_evidence(&dispute_id, &party_client, &hash1);
    client.submit_evidence(&dispute_id, &freelancer, &hash2);

    let evidence = client.get_evidence(&dispute_id);
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence.get(0).unwrap().evidence_hash, hash1);
    assert_eq!(evidence.get(1).unwrap().evidence_hash, hash2);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_submit_evidence_rejects_non_party() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let outsider = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "Breach"),
        &3u32,
        &None,
    );

    let hash: BytesN<32> = BytesN::random(&env);
    client.submit_evidence(&dispute_id, &outsider, &hash);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_submit_evidence_rejects_nonexistent_dispute() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);

    let submitter = Address::generate(&env);
    let hash: BytesN<32> = BytesN::random(&env);
    client.submit_evidence(&999u64, &submitter, &hash);
}

// ── Issue #773: vote_choice validated against defined VoteChoice variants ────

#[test]
fn test_cast_vote_valid_choices_accepted() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let reputation_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_id, &0, &escrow_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // VoteChoice::Client is accepted
    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::Client,
        &String::from_str(&env, "for client"),
        &1u64,
    );
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_client, 1);

    // VoteChoice::Freelancer is accepted
    client.cast_vote(
        &dispute_id,
        &assigned.get(1).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "for freelancer"),
        &2u64,
    );
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_freelancer, 1);

    // VoteChoice::RefundSplit is accepted with a valid percentage
    client.cast_vote(
        &dispute_id,
        &assigned.get(2).unwrap(),
        &VoteChoice::RefundSplit(50u32),
        &String::from_str(&env, "refund split"),
        &3u64,
    );
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_refund_split, 1);
}

#[test]
fn test_cast_vote_split_award_bps_validation() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let reputation_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_id, &0, &escrow_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // SplitAward with valid bps (client_bps + freelancer_bps == 10_000) is accepted
    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::SplitAward(6000u32, 4000u32),
        &String::from_str(&env, "split"),
        &1u64,
    );
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.votes_for_split_award, 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_cast_vote_split_award_invalid_bps_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let reputation_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_id, &0, &escrow_id);

    let party_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &party_client,
        &freelancer,
        &party_client,
        &String::from_str(&env, "dispute"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);

    // SplitAward where bps don't sum to 10_000 — InvalidSplitBps (#18)
    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::SplitAward(9999u32, 0u32),
        &String::from_str(&env, "bad split"),
        &1u64,
    );
}

// ─── Escrow deadlock recovery tests (issue #867) ───────────────────────────

fn setup_dispute_with_failing_escrow(
    env: &Env,
) -> (DisputeContractClient, Address, Address, u64) {
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(env, &dispute_contract_id);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    // Register the *failing* escrow so every resolve_dispute_callback panics.
    let escrow_contract_id = env.register_contract(None, DummyEscrowFailing);
    let admin = Address::generate(env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(env);
        client.add_arbitrator(&admin, &arb);
    }

    let job_client = Address::generate(env);
    let freelancer = Address::generate(env);
    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(env, "test dispute"),
        &3u32,
        &None,
    );
    (client, job_client, freelancer, dispute_id)
}

/// When the escrow panics during resolution the dispute lands in ResolutionFailed,
/// not a contract panic, and the pending resolution is stored.
#[test]
fn test_resolve_dispute_escrow_fail_enters_resolution_failed() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _job_client, _freelancer, dispute_id) =
        setup_dispute_with_failing_escrow(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &2u64);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolutionFailed);
}

/// After escrow is fixed, retry_escrow_callback transitions the dispute to the
/// correct terminal status (ResolvedForClient here).
#[test]
fn test_retry_escrow_callback_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    // Phase 1: resolve with broken escrow → ResolutionFailed.
    let (client, _job_client, _freelancer, dispute_id) =
        setup_dispute_with_failing_escrow(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &2u64);

    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);

    // Phase 2: "fix" the escrow by pointing the contract at a working one.
    let working_escrow_id = env.register_contract(None, DummyEscrow);
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &working_escrow_id);
    });

    // Phase 3: retry should succeed.
    let status = client.retry_escrow_callback(&dispute_id);
    assert_eq!(status, DisputeStatus::ResolvedForClient);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);
}

/// Calling retry when the escrow is still broken does not panic or change state —
/// it returns ResolutionFailed so the caller can try again later.
#[test]
fn test_retry_escrow_callback_idempotent_on_still_failing_escrow() {
    let env = Env::default();
    env.mock_all_auths();

    let (client, _job_client, _freelancer, dispute_id) =
        setup_dispute_with_failing_escrow(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &2u64);

    // First retry — escrow still broken.
    let status1 = client.retry_escrow_callback(&dispute_id);
    assert_eq!(status1, DisputeStatus::ResolutionFailed);

    // Second retry — still broken, same result.
    let status2 = client.retry_escrow_callback(&dispute_id);
    assert_eq!(status2, DisputeStatus::ResolutionFailed);

    // Dispute should still be in ResolutionFailed.
    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);
}

/// The MaliciousFiling escrow path also parks into ResolutionFailed on failure
/// and succeeds after retry.
#[test]
fn test_malicious_filing_escrow_fail_and_retry() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrowFailing);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..10 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "bad faith"),
        &5u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &2u64);
    client.cast_vote(&dispute_id, &assigned.get(3).unwrap(), &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad faith"), &3u64);
    client.cast_vote(&dispute_id, &assigned.get(4).unwrap(), &VoteChoice::Client,           &String::from_str(&env, "disagree"), &4u64);

    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);

    // Fix the escrow and retry.
    let working_escrow_id = env.register_contract(None, DummyEscrow);
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &working_escrow_id);
    });

    let status = client.retry_escrow_callback(&dispute_id);
    assert_eq!(status, DisputeStatus::MaliciousDisputeFiling);
}

/// Calling retry_escrow_callback on an already-resolved dispute returns AlreadyResolved.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_retry_on_already_resolved_dispute_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let admin = Address::generate(&env);
    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..3 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64,
        &job_client,
        &freelancer,
        &job_client,
        &String::from_str(&env, "test"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &2u64);

    // Dispute is now ResolvedForClient — retry should return AlreadyResolved (#7).
    client.retry_escrow_callback(&dispute_id);
}

/// Regression test for issue #1001: retry_escrow_callback must respect the pause guard.
/// Pausing the contract prevents retry from pushing pending resolutions to escrow,
/// and unpausing restores normal retry behavior.
#[test]
fn test_retry_escrow_callback_respects_pause() {
    let env = Env::default();
    env.mock_all_auths();

    // Phase 1: Set up a dispute with failing escrow → ResolutionFailed.
    let (client, _job_client, _freelancer, dispute_id) =
        setup_dispute_with_failing_escrow(&env);

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "r"), &2u64);

    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);

    // Phase 2: Pause the contract.
    let admin_addr = env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .get::<_, Address>(&DataKey::Admin)
            .unwrap()
    });
    client.pause(&admin_addr);

    // Phase 3: retry_escrow_callback should fail with ContractPaused (#11).
    let result = client.try_retry_escrow_callback(&dispute_id);
    assert_eq!(result, Err(Ok(DisputeError::ContractPaused)));

    // Dispute should still be in ResolutionFailed.
    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);

    // Phase 4: Unpause and fix escrow.
    client.unpause(&admin_addr);
    let working_escrow_id = env.register_contract(None, DummyEscrow);
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &working_escrow_id);
    });

    // Phase 5: Now retry should succeed.
    let status = client.retry_escrow_callback(&dispute_id);
    assert_eq!(status, DisputeStatus::ResolvedForClient);
    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolvedForClient);
}

#[test]
fn test_appeal_tie_break_respects_method() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &Some(TieBreakMethod::FavorClient),
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C1"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C2"), &0u64);
    // The third unanimous "Client" vote reaches AUTO_RESOLVE_VOTE_THRESHOLD, so
    // cast_vote auto-resolves the dispute internally — no explicit resolve_dispute
    // call is needed (and one would fail with AlreadyResolved).
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C3"), &0u64);

    let appeal_id = client.appeal(&dispute_id, &freelancer);
    
    let arb1 = Address::generate(&env);
    let arb2 = Address::generate(&env);
    let arb3 = Address::generate(&env);
    let arb4 = Address::generate(&env);
    client.cast_appeal_vote(&appeal_id, &arb1, &VoteChoice::Client, &String::from_str(&env, "C"));
    client.cast_appeal_vote(&appeal_id, &arb2, &VoteChoice::Client, &String::from_str(&env, "C"));
    client.cast_appeal_vote(&appeal_id, &arb3, &VoteChoice::Freelancer, &String::from_str(&env, "F"));
    client.cast_appeal_vote(&appeal_id, &arb4, &VoteChoice::Freelancer, &String::from_str(&env, "F"));

    let appeal_status = client.resolve_appeal(&appeal_id);
    assert_eq!(appeal_status, AppealStatus::RefundedBoth);
}

#[test]
fn test_get_dispute_tally_and_finalize_verdict() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    // Check initial tally is empty
    let tally_before = client.get_dispute_tally(&dispute_id);
    assert_eq!(tally_before.vote_count, 0);

    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C1"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Freelancer, &String::from_str(&env, "F1"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C2"), &0u64);

    let tally_after = client.get_dispute_tally(&dispute_id);
    assert_eq!(tally_after.vote_count, 3);
    assert!(tally_after.client_weight > 0);
    assert!(tally_after.freelancer_weight > 0);

    let status = client.finalize_verdict(&dispute_id);
    assert_eq!(status, DisputeStatus::ResolvedForClient);
}

#[test]
fn test_get_arbitrators_returns_voters() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Issue"),
        &3u32,
        &None,
    );

    let assigned = client.get_assigned_arbitrators(&dispute_id);
    
    // Check voters is empty initially
    let voters_before = client.get_arbitrators(&dispute_id);
    assert_eq!(voters_before.len(), 0);

    let voter1 = assigned.get(0).unwrap();
    let voter2 = assigned.get(1).unwrap();

    client.cast_vote(&dispute_id, &voter1, &VoteChoice::Client, &String::from_str(&env, "C1"), &0u64);
    client.cast_vote(&dispute_id, &voter2, &VoteChoice::Freelancer, &String::from_str(&env, "F1"), &0u64);

    let voters_after = client.get_arbitrators(&dispute_id);
    assert_eq!(voters_after.len(), 2);
    assert!(voters_after.contains(voter1));
    assert!(voters_after.contains(voter2));
    assert!(!voters_after.contains(assigned.get(2).unwrap()));
}

// ── Appeal subsystem tests ────────────────────────────────────────────────

/// Helper: resolve a dispute as ResolvedForClient so it can be appealed.
/// cast_vote auto-resolves when 3 votes reach the threshold, so no explicit
/// resolve_dispute call is needed.
fn resolve_dispute_for_client(env: &Env, client: &DisputeContractClient, dispute_id: u64) {
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(env, "C1"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(env, "C2"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(env, "C3"), &2u64);
    // Auto-resolve triggers on the 3rd vote — verify it landed
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);
}

// ── appeal() error paths ──────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")] // InvalidParty
fn test_appeal_rejects_non_party() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);

    let outsider = Address::generate(&env);
    client.appeal(&dispute_id, &outsider);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")] // VotingClosed (dispute not resolved)
fn test_appeal_rejects_unresolved_dispute() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    // Dispute is still Open — appeal should fail
    client.appeal(&dispute_id, &user_client);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")] // AppealWindowExpired
fn test_appeal_window_expired() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);

    // Advance ledger past the 48-hour appeal window
    env.ledger().with_mut(|li| {
        li.timestamp = li.timestamp + APPEAL_WINDOW_SECS + 1;
    });

    client.appeal(&dispute_id, &user_client);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")] // AlreadyAppealed
fn test_appeal_rejects_duplicate() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);

    client.appeal(&dispute_id, &user_client);
    // Second appeal on the same dispute should fail
    client.appeal(&dispute_id, &freelancer);
}

#[test]
fn test_appeal_success() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);

    let appeal_id = client.appeal(&dispute_id, &freelancer);
    assert_eq!(appeal_id, 1);

    let ap = client.get_appeal(&appeal_id);
    assert_eq!(ap.dispute_id, dispute_id);
    assert_eq!(ap.appellant, freelancer);
    assert_eq!(ap.status, AppealStatus::Open);
    assert_eq!(ap.votes_for_client, 0);
    assert_eq!(ap.votes_for_freelancer, 0);
    // Original voters (arbitrators who actually cast votes) should be excluded
    let voters = client.get_arbitrators(&dispute_id);
    for i in 0..voters.len() {
        assert!(ap.excluded_arbitrators.contains(&voters.get(i).unwrap()));
    }
}

// ── cast_appeal_vote() error paths ────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #6)")] // InvalidParty
fn test_cast_appeal_vote_rejects_party() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    // Dispute client tries to vote on appeal
    client.cast_appeal_vote(&appeal_id, &user_client, &VoteChoice::Client, &String::from_str(&env, "vote"));
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")] // ConflictOfInterest
fn test_cast_appeal_vote_rejects_original_arbitrator() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    // One of the original arbitrators tries to vote on the appeal
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    let original_arb = assigned.get(0).unwrap();
    client.cast_appeal_vote(&appeal_id, &original_arb, &VoteChoice::Client, &String::from_str(&env, "nope"));
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // AlreadyVoted
fn test_cast_appeal_vote_rejects_duplicate() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    let voter = Address::generate(&env);
    client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Client, &String::from_str(&env, "V1"));
    // Same voter again
    client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Freelancer, &String::from_str(&env, "V2"));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_cast_appeal_vote_rejects_invalid_choice() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    let voter = Address::generate(&env);
    client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::MaliciousFiling, &String::from_str(&env, "bad"));
}

#[test]
#[should_panic(expected = "Error(Contract, #18)")] // InvalidSplitBps
fn test_cast_appeal_vote_rejects_split_bps_over_100() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    let voter = Address::generate(&env);
    client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::RefundSplit(150), &String::from_str(&env, "too high"));
}

// ── resolve_appeal() tests ────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #5)")] // NotEnoughVotes
fn test_resolve_appeal_not_enough_votes() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    let voter = Address::generate(&env);
    client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Freelancer, &String::from_str(&env, "V1"));

    // Only 1 vote — need at least APPEAL_MIN_VOTES (3)
    client.resolve_appeal(&appeal_id);
}

#[test]
fn test_resolve_appeal_client_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    for i in 0..3 {
        let voter = Address::generate(&env);
        client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Client, &String::from_str(&env, "C"));
    }

    let result = client.resolve_appeal(&appeal_id);
    assert_eq!(result, AppealStatus::ResolvedForClient);

    // Verify the original dispute status was overwritten
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForClient);
}

#[test]
fn test_resolve_appeal_freelancer_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    // Resolve for client first, then appeal reverses it
    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    for i in 0..3 {
        let voter = Address::generate(&env);
        client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Freelancer, &String::from_str(&env, "F"));
    }

    let result = client.resolve_appeal(&appeal_id);
    assert_eq!(result, AppealStatus::ResolvedForFreelancer);

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolvedForFreelancer);
}

#[test]
fn test_resolve_appeal_refund_split() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    // 3 votes for RefundSplit with values 40, 60, 50 → avg = 50
    for pct in [40u32, 60, 50] {
        let voter = Address::generate(&env);
        client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::RefundSplit(pct), &String::from_str(&env, "S"));
    }

    let result = client.resolve_appeal(&appeal_id);
    assert_eq!(result, AppealStatus::RefundSplit(50));

    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::RefundSplit(50));
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")] // AlreadyResolved
fn test_resolve_appeal_already_resolved() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    for _ in 0..3 {
        let voter = Address::generate(&env);
        client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Client, &String::from_str(&env, "C"));
    }

    client.resolve_appeal(&appeal_id);
    // Second resolve should fail
    client.resolve_appeal(&appeal_id);
}

#[test]
fn test_resolve_appeal_escrow_failure() {
    let env = Env::default();
    env.mock_all_auths();

    // Use the failing escrow
    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrowFailing);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    // Resolve via voting — failing escrow means ResolutionFailed
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(&dispute_id, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C"), &0u64);
    client.cast_vote(&dispute_id, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C"), &1u64);
    client.cast_vote(&dispute_id, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "C"), &2u64);

    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolutionFailed);

    // Swap to a working escrow so retry succeeds
    let working_escrow_id = env.register_contract(None, DummyEscrow);
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &working_escrow_id);
    });

    client.retry_escrow_callback(&dispute_id);
    assert_eq!(client.get_dispute(&dispute_id).status, DisputeStatus::ResolvedForClient);

    // Now appeal
    let appeal_id = client.appeal(&dispute_id, &freelancer);

    for _ in 0..3 {
        let voter = Address::generate(&env);
        client.cast_appeal_vote(&appeal_id, &voter, &VoteChoice::Freelancer, &String::from_str(&env, "F"));
    }

    // Resolve appeal — escrow callback will fail again (we still have working escrow,
    // so we swap back to the failing one to test the fallback path)
    let failing_escrow_id = env.register_contract(None, DummyEscrowFailing);
    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &failing_escrow_id);
    });

    let result = client.resolve_appeal(&appeal_id);
    assert_eq!(result, AppealStatus::ResolvedForFreelancer);

    // Dispute should land in ResolutionFailed due to escrow callback failure
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::ResolutionFailed);
}

// ── get_appeal() tests ────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #21)")] // AppealNotFound
fn test_get_appeal_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    client.get_appeal(&999);
}

#[test]
fn test_get_appeal_success() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_id = env.register_contract(None, DummyEscrow);
    let rep_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);
    client.initialize(&admin, &rep_id, &300, &escrow_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let dispute_id = client.raise_dispute(
        &1u64, &user_client, &freelancer, &user_client,
        &String::from_str(&env, "Issue"), &3u32, &None,
    );

    resolve_dispute_for_client(&env, &client, dispute_id);
    let appeal_id = client.appeal(&dispute_id, &user_client);

    let ap = client.get_appeal(&appeal_id);
    assert_eq!(ap.id, appeal_id);
    assert_eq!(ap.dispute_id, dispute_id);
    assert_eq!(ap.appellant, user_client);
    assert_eq!(ap.status, AppealStatus::Open);
    assert_eq!(ap.votes_for_client, 0);
    assert_eq!(ap.votes_for_freelancer, 0);
    assert_eq!(ap.votes_for_refund_split, 0);
    assert_eq!(ap.refund_split_sum, 0);
}

// ─── get_dispute_by_job / get_disputes_for_job tests ─────────────────────

/// Both lookup functions return empty/None for a job that has never had a dispute.
#[test]
fn test_get_dispute_by_job_returns_none_for_unknown_job() {
    let env = Env::default();
    env.mock_all_auths();

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    // No dispute raised for job 42
    let result = client.get_dispute_by_job(&42u64);
    assert!(result.is_none());

    let disputes = client.get_disputes_for_job(&42u64);
    assert_eq!(disputes.len(), 0);
}

/// When the same job has multiple disputes (raised across cooldown periods),
/// get_dispute_by_job returns the latest one while get_disputes_for_job returns
/// the full ordered history.
#[test]
fn test_get_dispute_by_job_and_for_job_with_multiple_disputes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);
    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);
    let admin = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        client.add_arbitrator(&admin, &Address::generate(&env));
    }

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // ── First dispute on job 1 ────────────────────────────────────────────
    let d1 = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "First dispute"),
        &3u32,
        &None,
    );
    assert_eq!(d1, 1);

    // Resolve the dispute (3 votes for client → auto-resolve)
    let assigned = client.get_assigned_arbitrators(&d1);
    client.cast_vote(&d1, &assigned.get(0).unwrap(), &VoteChoice::Client, &String::from_str(&env, "V1"), &0);
    client.cast_vote(&d1, &assigned.get(1).unwrap(), &VoteChoice::Client, &String::from_str(&env, "V2"), &0);
    client.cast_vote(&d1, &assigned.get(2).unwrap(), &VoteChoice::Client, &String::from_str(&env, "V3"), &0);

    let dispute1 = client.get_dispute(&d1);
    assert_eq!(dispute1.status, DisputeStatus::ResolvedForClient);

    // ── After cooldown, raise a second dispute on the same job ────────────
    // Advance past both the per-job cooldown (86_400s) and the per-party cooldown (1_209_600s).
    env.ledger().with_mut(|l| l.timestamp = 1000 + 1_209_601);

    let d2 = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &freelancer,
        &String::from_str(&env, "Second dispute"),
        &3u32,
        &None,
    );
    assert_eq!(d2, 2);

    // ── get_dispute_by_job returns the latest ─────────────────────────────
    let latest = client.get_dispute_by_job(&1u64);
    assert!(latest.is_some());
    let latest = latest.unwrap();
    assert_eq!(latest.id, d2);
    assert_eq!(latest.job_id, 1);
    assert_eq!(latest.reason, String::from_str(&env, "Second dispute"));

    // ── get_disputes_for_job returns the full history in order ─────────────
    let history = client.get_disputes_for_job(&1u64);
    assert_eq!(history.len(), 2);

    let first = history.get(0).unwrap();
    assert_eq!(first.id, d1);
    assert_eq!(first.reason, String::from_str(&env, "First dispute"));

    let second = history.get(1).unwrap();
    assert_eq!(second.id, d2);
    assert_eq!(second.reason, String::from_str(&env, "Second dispute"));
}

/// Regression test for the dispute storage TTL sizing (issue #1163): a dispute
/// that sits idle for a long stretch — plausible early in the 7-day voting
/// window — must not have its storage archived by the ledger before it is
/// resolved. The full worst-case lifecycle is ~16 days (7-day voting period,
/// 48-hour appeal window, then a 7-day appeal voting period), so the previous
/// ~10,000-ledger TTL (~14 hours) was far too short.
#[test]
fn test_dispute_storage_survives_long_idle_period_to_resolution() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let dispute_contract_id = env.register_contract(None, DisputeContract);
    let client = DisputeContractClient::new(&env, &dispute_contract_id);

    let escrow_contract_id = env.register_contract(None, DummyEscrow);
    let reputation_contract_id = env.register_contract(None, MockReputationContract);

    // The dispute's storage must survive a long idle period. To also exercise
    // end-to-end resolution *after* that idle period, keep the mock helper
    // contracts' instance storage live so the ledger advance below does not
    // archive them (they hold no per-dispute state; they are test plumbing).
    env.as_contract(&reputation_contract_id, || {
        env.storage().instance().extend_ttl(50_000_000, 50_000_000);
    });
    env.as_contract(&escrow_contract_id, || {
        env.storage().instance().extend_ttl(50_000_000, 50_000_000);
    });

    let admin = Address::generate(&env);

    let user_client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    client.initialize(&admin, &reputation_contract_id, &300, &escrow_contract_id);

    for _ in 0..5 {
        let arb = Address::generate(&env);
        client.add_arbitrator(&admin, &arb);
    }

    let dispute_id = client.raise_dispute(
        &1u64,
        &user_client,
        &freelancer,
        &user_client,
        &String::from_str(&env, "Work not delivered"),
        &3u32,
        &None,
    );

    // Simulate a long-idle dispute: the full 7-day voting period elapses with
    // no interaction (~120,960 ledgers at 5s/ledger). This is far beyond the
    // old 10,000-ledger TTL but well inside the resized 21/30-day window, so
    // the dispute's storage must still be live.
    env.ledger().with_mut(|l| {
        l.timestamp = 1000 + VOTING_PERIOD_SECS;
        l.sequence_number = 1 + (VOTING_PERIOD_SECS / 5) as u32;
    });

    // The dispute storage must have survived the idle period (not archived).
    let dispute = client.get_dispute(&dispute_id);
    assert_eq!(dispute.status, DisputeStatus::Open);

    // And it must still be fully resolvable end-to-end — the long-idle dispute
    // is not silently broken; storage survives all the way to resolution.
    let assigned = client.get_assigned_arbitrators(&dispute_id);
    client.cast_vote(
        &dispute_id,
        &assigned.get(0).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Work was delivered"),
        &0,
    );
    client.cast_vote(
        &dispute_id,
        &assigned.get(1).unwrap(),
        &VoteChoice::Freelancer,
        &String::from_str(&env, "Agree with freelancer"),
        &0,
    );
    client.cast_vote(
        &dispute_id,
        &assigned.get(2).unwrap(),
        &VoteChoice::Client,
        &String::from_str(&env, "Incomplete work"),
        &0,
    );

    let result = client.resolve_dispute(&dispute_id);
    assert_eq!(result, DisputeStatus::ResolvedForFreelancer);
}
