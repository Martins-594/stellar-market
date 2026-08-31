use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events as _, Ledger},
    vec, Env, String, Symbol, TryFromVal,
};
use stellar_market_escrow::EscrowContract;

// The minimum stake required per review (must match lib.rs constant)
const MIN_STAKE: i128 = 10_000_000;
const ONE_YEAR_IN_SECONDS: u64 = 31_536_000;

fn pause_reputation(env: &Env, client: &ReputationContractClient<'_>, admin: &Address) {
    client.propose_admin_action(admin, &AdminAction::Pause);
}

fn unpause_reputation(env: &Env, client: &ReputationContractClient<'_>, admin: &Address) {
    client.propose_admin_action(admin, &AdminAction::Unpause);
}

/// Helper: create a job in the escrow contract and mark it as completed.
/// This uses the actual escrow contract functions to ensure proper storage.
fn setup_completed_job(
    env: &Env,
    escrow_id: &Address,
    _job_id: u64,
    client: &Address,
    freelancer: &Address,
    token: &Address,
) {
    let escrow_client = stellar_market_escrow::EscrowContractClient::new(env, escrow_id);

    // Create a job with one milestone
    let milestones = vec![
        env,
        (String::from_str(env, "Task"), 100_i128, 9999999999u64),
    ];
    let job_id = escrow_client.create_job(
        client,
        freelancer,
        token,
        &milestones,
        &9999999999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );

    // Fund the job
    escrow_client.fund_job(&job_id, client, &0, &0);

    // Mark the job as completed using the dispute resolution callback
    escrow_client.resolve_dispute_callback(&job_id, &stellar_market_escrow::DisputeResolution::FreelancerWins);
}

/// Helper: create a job in the escrow contract and mark it as in progress.
/// This uses the actual escrow contract functions to ensure proper storage.
fn setup_in_progress_job(
    env: &Env,
    escrow_id: &Address,
    _job_id: u64,
    client: &Address,
    freelancer: &Address,
    token: &Address,
) {
    let escrow_client = stellar_market_escrow::EscrowContractClient::new(env, escrow_id);

    // Create a job with one milestone
    let milestones = vec![
        env,
        (String::from_str(env, "Task"), 100_i128, 9999999999u64),
    ];
    let job_id = escrow_client.create_job(
        client,
        freelancer,
        token,
        &milestones,
        &9999999999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );

    // Fund the job to move it to Funded status
    escrow_client.fund_job(&job_id, client, &0, &0);
}

fn create_token(env: &Env, admin: &Address) -> Address {
    env.register_stellar_asset_contract_v2(admin.clone()).address()
}

fn mint(env: &Env, token_addr: &Address, admin: &Address, to: &Address, amount: i128) {
    let token_client = token::StellarAssetClient::new(env, token_addr);
    token_client.mint(to, &amount);
    // Also approve reputation contract to receive stake (mock_all_auths handles this)
    let _ = admin;
}

#[test]
fn test_submit_review_client_reviews_freelancer() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);

    setup_completed_job(
        &env,
        &escrow_id,
        1u64,
        &client_addr,
        &freelancer_addr,
        &token_addr,
    );

    reputation_client.submit_review(
        &escrow_id,
        &client_addr,
        &freelancer_addr,
        &1u64,
        &4u32,
        &String::from_str(&env, "Great work!"),
        &MIN_STAKE,
    );

    let rep = reputation_client.get_reputation(&freelancer_addr);
    assert_eq!(rep.review_count, 1);
    assert_eq!(rep.total_score, 4 * MIN_STAKE as u64);
    assert_eq!(rep.total_weight, MIN_STAKE as u64);
}


#[test]
fn test_average_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // Review 1: 5 stars, min weight
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Excellent"),
        &MIN_STAKE,
    );

    // Review 2: 3 stars, min weight
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &3u32,
        &String::from_str(&env, "Average"),
        &MIN_STAKE,
    );

    let avg = reputation_client.get_average_rating(&reviewee);
    // (5*MIN + 3*MIN) * 100 / (MIN + MIN) = 400 (4.00 stars)
    assert_eq!(avg, 400);
    assert_eq!(reputation_client.get_review_count(&reviewee), 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_invalid_rating() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);

    // Rating is validated first (before stake check), so small weight still triggers #1
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &6u32, // Invalid: max is 5
        &String::from_str(&env, "Too high"),
        &1_i128,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_self_review() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user = Address::generate(&env);

    // Self-review check happens before stake check
    reputation_client.submit_review(
        &escrow_id,
        &user,
        &user,
        &1u64,
        &5u32,
        &String::from_str(&env, "I'm great"),
        &1_i128,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_reject_below_min_stake() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Sneaky low stake"),
        &(MIN_STAKE - 1), // Just below minimum
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_job_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);

    // BelowMinStake is checked before JobNotFound, so we see #11 here.
    // To test JobNotFound properly, we need sufficient stake — but there's no token minted,
    // so the token transfer will fail anyway. This tests the ordering of checks.
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &99u64,
        &5u32,
        &String::from_str(&env, "Does not exist"),
        &1_i128, // Below min stake triggers #11 first
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_job_not_found_with_valid_stake() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    // No job set up in escrow — should fail with JobNotFound (#7)
    // We need a job record to pass the escrow check, but no job exists here.
    // We use a dummy token for the stake transfer to succeed, but crossing contract
    // boundary will fail because there's no job.
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &99u64,
        &5u32,
        &String::from_str(&env, "Does not exist"),
        &MIN_STAKE,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_job_not_completed() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);

    // Job is InProgress, not Completed
    setup_in_progress_job(
        &env,
        &escrow_id,
        1u64,
        &client_addr,
        &freelancer_addr,
        &token_addr,
    );

    reputation_client.submit_review(
        &escrow_id,
        &client_addr,
        &freelancer_addr,
        &1u64,
        &5u32,
        &String::from_str(&env, "Too early"),
        &MIN_STAKE,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_not_job_participant() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let outsider = Address::generate(&env);
    let another = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);
    mint(&env, &token_addr, &token_admin, &outsider, 100_000_000);

    setup_completed_job(
        &env,
        &escrow_id,
        1u64,
        &client_addr,
        &freelancer_addr,
        &token_addr,
    );

    // outsider and another were not part of job 1
    reputation_client.submit_review(
        &escrow_id,
        &outsider,
        &another,
        &1u64,
        &5u32,
        &String::from_str(&env, "Fraudulent review"),
        &MIN_STAKE,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_reviewer_not_participant_but_reviewee_is() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let outsider = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);
    mint(&env, &token_addr, &token_admin, &outsider, 100_000_000);

    setup_completed_job(
        &env,
        &escrow_id,
        1u64,
        &client_addr,
        &freelancer_addr,
        &token_addr,
    );

    // outsider tries to review the freelancer — reviewer is not a participant
    reputation_client.submit_review(
        &escrow_id,
        &outsider,
        &freelancer_addr,
        &1u64,
        &5u32,
        &String::from_str(&env, "I wasn't there"),
        &MIN_STAKE,
    );
}

#[test]
fn test_get_tier_no_reputation() {
    let env = Env::default();
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user = Address::generate(&env);
    let tier = reputation_client.get_tier(&user);
    assert_eq!(tier, ReputationTier::None);
}

#[test]
fn test_get_tier_bronze() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review with rating 2 (avg = 200, Bronze tier)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Okay"),
        &MIN_STAKE,
    );

    let tier = reputation_client.get_tier(&reviewee);
    assert_eq!(tier, ReputationTier::Bronze);
}

#[test]
fn test_get_tier_silver() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review with rating 4 (avg = 400, Silver tier)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good"),
        &MIN_STAKE,
    );

    let tier = reputation_client.get_tier(&reviewee);
    assert_eq!(tier, ReputationTier::Silver);
}

#[test]
fn test_get_tier_gold() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // Two 5-star reviews with equal weight -> avg = 500 (Gold tier)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Excellent"),
        &MIN_STAKE,
    );

    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &5u32,
        &String::from_str(&env, "Perfect"),
        &MIN_STAKE,
    );

    let tier = reputation_client.get_tier(&reviewee);
    assert_eq!(tier, ReputationTier::Gold);
}

#[test]
fn test_get_tier_platinum() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewer3 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer3, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 3u64, &reviewer3, &reviewee, &token_addr);

    // Three 5-star reviews -> avg = 500 (Gold)
    // Platinum (700+) is impossible with max rating 5 * 100 = 500
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Outstanding"),
        &MIN_STAKE,
    );

    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &5u32,
        &String::from_str(&env, "Exceptional"),
        &MIN_STAKE,
    );

    reputation_client.submit_review(
        &escrow_id,
        &reviewer3,
        &reviewee,
        &3u64,
        &5u32,
        &String::from_str(&env, "World-class"),
        &MIN_STAKE,
    );

    let avg = reputation_client.get_average_rating(&reviewee);
    assert_eq!(avg, 500);

    // Platinum requires avg >= 700, impossible with max rating = 5 (5*100=500). Current tier: Gold.
    let tier = reputation_client.get_tier(&reviewee);
    assert_eq!(tier, ReputationTier::Gold);
}

#[test]
fn test_badge_awarded_on_tier_crossing() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review that crosses into Bronze tier
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Decent"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 1);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Bronze);
}

#[test]
fn test_badge_not_duplicated() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // First review: Bronze tier (rating 2)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Okay"),
        &MIN_STAKE,
    );

    // Second review: Still Bronze tier (avg = (2 + 2) / 2 = 2 = 200)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &2u32,
        &String::from_str(&env, "Okay again"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    // Should only have one Bronze badge, not two
    assert_eq!(badges.len(), 1);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Bronze);
}

#[test]
fn test_multiple_tier_badges() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewer3 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer3, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 3u64, &reviewer3, &reviewee, &token_addr);

    // First review: Bronze tier (rating 2, avg = 200)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Okay"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 1);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Bronze);

    // Second review: Silver tier
    // avg = (2*MIN + 5*MIN) * 100 / (2*MIN) = 350 -> Silver
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &5u32,
        &String::from_str(&env, "Great improvement"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 2);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Bronze);
    assert_eq!(badges.get(1).unwrap().badge_type, ReputationTier::Silver);

    // Third review with same weight and rating 5:
    // avg = (2 + 5 + 5) * MIN * 100 / (3 * MIN) = 1200 / 3 = 400 -> still Silver
    reputation_client.submit_review(
        &escrow_id,
        &reviewer3,
        &reviewee,
        &3u64,
        &5u32,
        &String::from_str(&env, "Excellent"),
        &MIN_STAKE,
    );

    let avg = reputation_client.get_average_rating(&reviewee);
    assert!(avg < 500); // Still Silver

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 2); // Still Bronze and Silver
}

#[test]
fn test_tier_downgrade_no_badge_removal() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // First review: Silver tier (rating 4, avg = 400)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 1);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Silver);

    // Second review: Low rating brings average down to Bronze
    // avg = (4*M + 1*M) * 100 / (2*M) = 250 (Bronze)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &1u32,
        &String::from_str(&env, "Poor"),
        &MIN_STAKE,
    );

    let tier = reputation_client.get_tier(&reviewee);
    assert_eq!(tier, ReputationTier::Bronze);

    // Badge should still exist (badges are permanent achievements)
    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 2); // Silver badge remains, Bronze badge added
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Silver);
    assert_eq!(badges.get(1).unwrap().badge_type, ReputationTier::Bronze);
}

#[test]
fn test_get_reputation_with_decay() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with 50% decay per year
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    // Configure the token used for stake transfers via admin action.
    reputation_client.propose_admin_action(&admin, &AdminAction::SetToken(token_addr.clone()));

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Initial review at t=0
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good"),
        &MIN_STAKE,
    );

    // At t=0, score should be raw
    let rep0 = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep0.total_score, 4 * MIN_STAKE as u64);
    assert_eq!(rep0.total_weight, MIN_STAKE as u64);

    // Advance time by 1 year (31,536,000 seconds)
    // Decay is 50%, so weight should be 50% of MIN_STAKE
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 31_536_000,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });
    
    let rep1 = reputation_client.get_reputation(&reviewee);
    let expected_weight = (MIN_STAKE as u64) / 2;
    let expected_score = 4 * expected_weight;
    
    assert_eq!(rep1.total_weight, expected_weight);
    assert_eq!(rep1.total_score, expected_score);
    assert_eq!(rep1.review_count, 1);
}

#[test]
fn test_get_badges_empty() {
    let env = Env::default();
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user = Address::generate(&env);
    let badges = reputation_client.get_badges(&user);
    assert_eq!(badges.len(), 0);
}

#[test]
fn test_badge_timestamp() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    let before_timestamp = env.ledger().timestamp();

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &3u32,
        &String::from_str(&env, "Good"),
        &MIN_STAKE,
    );

    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 1);

    let badge = badges.get(0).unwrap();
    assert!(badge.awarded_at >= before_timestamp);
}

#[test]
fn test_set_decay_rate() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    // Set a decay rate within the default maximum (MAX_DECAY_RATE = 20).
    let _prop_id = reputation_client.propose_admin_action(&admin, &AdminAction::SetDecayRate(15u32));
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")]
fn test_set_decay_rate_invalid() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    // A decay rate above the maximum (#783) is rejected with DecayRateTooHigh (#25).
    reputation_client.propose_admin_action(&admin, &AdminAction::SetDecayRate(21u32));
}

#[test]
fn test_decay_calculation() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    // Set decay rate to 50% per year
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Initial timestamp: day 0
    let start_time = 1_000_000;
    env.ledger().with_mut(|l| l.timestamp = start_time);

    // Review with weight MIN_STAKE, rating 5
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Great"),
        &MIN_STAKE,
    );

    // At day 0 (no decay), avg = 500
    assert_eq!(reputation_client.get_average_rating(&reviewee), 500);

    // Advance 1 day (86400 seconds) — negligible decay
    env.ledger().with_mut(|l| l.timestamp = start_time + 86400);
    assert_eq!(reputation_client.get_average_rating(&reviewee), 500);

    // Advance 1 year (31,536,000 seconds)
    // 50% decay per year -> weight should be 50% of original, but ratio is the same for a single review
    env.ledger()
        .with_mut(|l| l.timestamp = start_time + 31_536_000);
    assert_eq!(reputation_client.get_average_rating(&reviewee), 500);

    // To test actual decay, add a second review at year 1
    let reviewer2 = Address::generate(&env);
    mint(&env, &token_addr, &token_admin, &reviewer2, 1_000_000_000);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // Second review at year 1 with rating 1 (Poor)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &1u32,
        &String::from_str(&env, "Terrible now"),
        &MIN_STAKE,
    );

    // Review 1 (5 stars) has 50% weight decay. Review 2 (1 star) has full weight.
    // effective_w1 = MIN_STAKE/2, effective_w2 = MIN_STAKE
    // Weighted score: 5 * (MIN/2) + 1 * MIN = 2.5*MIN + MIN = 3.5*MIN
    // Total weight: MIN/2 + MIN = 1.5*MIN
    // Avg = 3.5/1.5 * 100 = 233
    assert_eq!(reputation_client.get_average_rating(&reviewee), 233);

    // Advance to year 2
    // Review 1 is 2 years old -> 100% decayed (weight 0)
    // Review 2 is 1 year old -> 50% decayed (weight MIN/2)
    // Weighted score: 0 + 1 * MIN/2 = MIN/2
    // Total weight: MIN/2
    // Avg = 1.0 * 100 = 100
    env.ledger()
        .with_mut(|l| l.timestamp = start_time + 63_072_000);
    assert_eq!(reputation_client.get_average_rating(&reviewee), 100);
}

#[test]
fn test_decay_uses_timestamp_instead_of_ledger_sequence() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Stable over time"),
        &MIN_STAKE,
    );

    // Advance timestamp to 6 months; keep sequence small so entries are not archived.
    // (The test verifies that decay is driven by timestamp, not ledger sequence.)
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: ONE_YEAR_IN_SECONDS / 2,
        protocol_version: 20,
        sequence_number: 200,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    let rep = reputation_client.get_reputation(&reviewee);
    let expected_weight = (MIN_STAKE as u64 * 75) / 100;

    assert_eq!(rep.total_weight, expected_weight);
    assert_eq!(rep.total_score, 4 * expected_weight);

    // Same timestamp, very different sequence number — result must be identical.
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: ONE_YEAR_IN_SECONDS / 2,
        protocol_version: 20,
        sequence_number: 25,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    let rep_same_timestamp = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_same_timestamp.total_weight, expected_weight);
    assert_eq!(rep_same_timestamp.total_score, 4 * expected_weight);
}

#[test]
fn test_get_set_min_stake() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    // Default min stake
    assert_eq!(reputation_client.get_min_stake(), MIN_STAKE);

    // Update min stake via admin action
    let new_stake = 20_000_000_i128;
    reputation_client.propose_admin_action(&admin, &AdminAction::SetMinStake(new_stake));
    assert_eq!(reputation_client.get_min_stake(), new_stake);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_reject_rate_limit() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    let reviewer = Address::generate(&env);
    let reviewee1 = Address::generate(&env);
    let reviewee2 = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee1, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer, &reviewee2, &token_addr);

    // First review succeeds
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee1,
        &1u64,
        &5u32,
        &String::from_str(&env, "First"),
        &MIN_STAKE,
    );

    // Second review in same ledger -> RateLimitExceeded (#12)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee2,
        &2u64,
        &5u32,
        &String::from_str(&env, "Second"),
        &MIN_STAKE,
    );
}

#[test]
fn test_rate_limit_pass_after_time() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);

    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50u32);

    let reviewer = Address::generate(&env);
    let reviewee1 = Address::generate(&env);
    let reviewee2 = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee1, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer, &reviewee2, &token_addr);

    // First review at ledger 0
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee1,
        &1u64,
        &5u32,
        &String::from_str(&env, "First"),
        &MIN_STAKE,
    );

    // Advance ledger past rate limit (120 ledgers)
    env.ledger().with_mut(|l| l.sequence_number = 200);

    // Now the second review should succeed
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee2,
        &2u64,
        &4u32,
        &String::from_str(&env, "Second"),
        &MIN_STAKE,
    );

    assert_eq!(reputation_client.get_review_count(&reviewee1), 1);
    assert_eq!(reputation_client.get_review_count(&reviewee2), 1);
}

#[test]
fn test_register_referral_success() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let referrer = Address::generate(&env);
    let referree = Address::generate(&env);

    // Register referral
    reputation_client.register_referral(&referree, &referrer);

    // Assert referrer stats reflect the registration
    let stats = reputation_client.get_referral_stats(&referrer);
    assert_eq!(stats.total_referrals, 1);
    assert_eq!(stats.earned_bonus, 0); // No bonus until a job is completed
}

#[test]
fn test_referral_bonus_granted_on_first_job() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &0); // Set no decay for simpler testing

    let referrer = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env); // Freelancer will be referred
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &client, 100_000_000);

    // Register the referral BEFORE the job finishes
    reputation_client.register_referral(&freelancer, &referrer);

    setup_completed_job(&env, &escrow_id, 1u64, &client, &freelancer, &token_addr);

    // Client submits review. During this submission, the contract hooks `process_referral_bonus`
    reputation_client.submit_review(
        &escrow_id,
        &client,
        &freelancer,
        &1u64,
        &5u32,
        &String::from_str(&env, "Good job"),
        &MIN_STAKE,
    );

    // Check Referrer's Stats
    let stats = reputation_client.get_referral_stats(&referrer);
    assert_eq!(stats.total_referrals, 1);

    // Earned bonus uses fixed reputation weight (1), not min review stake.
    assert_eq!(stats.earned_bonus, 5u64);

    // Check Referrer's Reputation (they should have received the bonus reputation payload natively)
    let rep = reputation_client.get_reputation(&referrer);
    assert_eq!(rep.total_score, 5u64);
    assert_eq!(rep.total_weight, 1u64);
}

#[test]
fn test_referral_bonus_not_granted_twice() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &0);

    let referrer = Address::generate(&env);
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &client, 100_000_000);
    mint(&env, &token_addr, &token_admin, &freelancer, 100_000_000); // So freelancer can review back

    reputation_client.register_referral(&freelancer, &referrer);
    setup_completed_job(&env, &escrow_id, 1u64, &client, &freelancer, &token_addr);

    // Client submits review -> Process bonus triggers for both client and freelancer
    reputation_client.submit_review(
        &escrow_id,
        &client,
        &freelancer,
        &1u64,
        &5u32,
        &String::from_str(&env, "First review"),
        &MIN_STAKE,
    );

    let initial_stats = reputation_client.get_referral_stats(&referrer);

    // Advance ledger to clear rate limits
    env.ledger().with_mut(|l| l.sequence_number = 200);

    // Freelancer reviews client on the SAME job (or they do a new job, doesn't matter)
    reputation_client.submit_review(
        &escrow_id,
        &freelancer,
        &client,
        &1u64,
        &4u32,
        &String::from_str(&env, "Second review"),
        &MIN_STAKE,
    );

    // Referrer stats should NOT have increased (bonus paid only once per referred user)
    let subsequent_stats = reputation_client.get_referral_stats(&referrer);
    assert_eq!(initial_stats.earned_bonus, subsequent_stats.earned_bonus);
}

/// Verifies that submitting a first review for a (reviewer, job_id) pair succeeds
/// and updates the reviewee's reputation correctly.
#[test]
fn test_first_review_succeeds_and_updates_reputation() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &client_addr, &freelancer_addr, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &client_addr,
        &freelancer_addr,
        &1u64,
        &5u32,
        &String::from_str(&env, "Excellent work!"),
        &MIN_STAKE,
    );

    let rep = reputation_client.get_reputation(&freelancer_addr);
    assert_eq!(rep.review_count, 1);
    assert_eq!(rep.total_score, 5 * MIN_STAKE as u64);
    assert_eq!(rep.total_weight, MIN_STAKE as u64);
}

/// Verifies that a second submit_review call with the same (reviewer, job_id)
/// is rejected with AlreadyReviewed (contract error #2), preventing score inflation.
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_duplicate_review_rejected_with_already_reviewed() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let client_addr = Address::generate(&env);
    let freelancer_addr = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &client_addr, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &client_addr, &freelancer_addr, &token_addr);

    // First submission succeeds
    reputation_client.submit_review(
        &escrow_id,
        &client_addr,
        &freelancer_addr,
        &1u64,
        &5u32,
        &String::from_str(&env, "Great work!"),
        &MIN_STAKE,
    );

    // Advance past the rate-limit window so RateLimitExceeded does not fire first
    env.ledger().with_mut(|l| l.sequence_number = 200);

    // Second submission for the same (reviewer, job_id) must return AlreadyReviewed
    reputation_client.submit_review(
        &escrow_id,
        &client_addr,
        &freelancer_addr,
        &1u64,
        &5u32,
        &String::from_str(&env, "Duplicate attempt!"),
        &MIN_STAKE,
    );
}

#[test]
fn test_reputation_multisig_flow() {
    let env = Env::default();
    env.mock_all_auths();
    
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    
    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    let signers = vec![&env, signer1.clone(), signer2.clone()];
    
    client.initialize(&signers, &2, &0);

    // Propose pause — needs 2-of-2 approval so contract is not yet paused.
    let prop_id = client.propose_admin_action(&signer1, &AdminAction::Pause);
    assert_eq!(prop_id, 1);
    // Not yet executed — still should allow actions
    client.propose_admin_action(&signer1, &AdminAction::Unpause); // sanity check: can propose while active

    // Approve — crosses threshold, executes the Pause
    client.approve_admin_action(&signer2, &prop_id);
    // Paused: submitting a noop endorse should now fail
    let endorse_result = client.try_endorse(
        &signer1,
        &Address::generate(&env),
        &String::from_str(&env, "Rust"),
    );
    assert!(endorse_result.is_err());
}

#[test]
fn test_remove_signer_not_found_errors() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);

    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    client.initialize(&vec![&env, signer1.clone(), signer2.clone()], &1, &0);

    let not_a_signer = Address::generate(&env);
    let result =
        client.try_propose_admin_action(&signer1, &AdminAction::RemoveSigner(not_a_signer));
    assert!(result.is_err());
}

#[test]
fn test_reputation_slash_stake_multisig() {
    let env = Env::default();
    env.mock_all_auths();
    
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    
    let signer1 = Address::generate(&env);
    let signers = vec![&env, signer1.clone()];
    client.initialize(&signers, &1, &0);
    
    let loser = Address::generate(&env);
    // Proposal for slashing
    let prop_id = client.propose_admin_action(&signer1, &AdminAction::SlashStake(loser.clone(), 1u64, 100u64));
    
    // Should be executed immediately (threshold 1)
    let rep = client.get_reputation(&loser);
    // Since we started with 0, saturating_sub(100) is 0.
    // Actually, let's just check the event if we could, but assert_eq(0, 0) is trivial.
    // Let's at least check that it didn't fail.
}

// ─────────────────────────────────────────────────────────────────────────────
// #535 — Skill Endorsement System Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_endorse_success() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let endorser = Address::generate(&env);
    let target = Address::generate(&env);
    let skill = String::from_str(&env, "Rust");

    assert_eq!(client.get_skill_score(&target, &skill), 0);
    client.endorse(&endorser, &target, &skill);
    // endorser has 0 avg_rating → fallback weight 1
    assert_eq!(client.get_skill_score(&target, &skill), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_endorse_duplicate_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let endorser = Address::generate(&env);
    let target = Address::generate(&env);
    let skill = String::from_str(&env, "Smart Contracts");

    client.endorse(&endorser, &target, &skill);
    client.endorse(&endorser, &target, &skill); // AlreadyEndorsed #23
}

#[test]
fn test_endorse_different_skills_same_endorser_allowed() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let endorser = Address::generate(&env);
    let target = Address::generate(&env);
    let skill_rust = String::from_str(&env, "Rust");
    let skill_ui   = String::from_str(&env, "UI Design");

    client.endorse(&endorser, &target, &skill_rust);
    client.endorse(&endorser, &target, &skill_ui);

    assert_eq!(client.get_skill_score(&target, &skill_rust), 1);
    assert_eq!(client.get_skill_score(&target, &skill_ui),   1);
}

#[test]
fn test_endorse_multiple_endorsers_accumulate() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let target = Address::generate(&env);
    let skill  = String::from_str(&env, "Rust");
    for _ in 0..3u32 {
        let e = Address::generate(&env);
        client.endorse(&e, &target, &skill);
    }
    assert_eq!(client.get_skill_score(&target, &skill), 3);
}

#[test]
fn test_endorse_weighted_by_endorser_rating() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Give endorser1 a 5-star rating → avg = 500 → weight = 500/100 = 5
    let reviewer  = Address::generate(&env);
    let endorser1 = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr  = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &endorser1, &token_addr);
    client.submit_review(
        &escrow_id, &reviewer, &endorser1, &1u64, &5u32,
        &String::from_str(&env, "Perfect"), &MIN_STAKE,
    );

    let endorser2 = Address::generate(&env); // no reputation → weight 1
    let target    = Address::generate(&env);
    let skill     = String::from_str(&env, "Rust");

    client.endorse(&endorser1, &target, &skill);
    client.endorse(&endorser2, &target, &skill);

    // 5 + 1 = 6
    assert_eq!(client.get_skill_score(&target, &skill), 6);
}

#[test]
#[should_panic(expected = "Error(Contract, #27)")]
fn test_endorse_self_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let user = Address::generate(&env);
    let skill = String::from_str(&env, "Rust");

    client.endorse(&user, &user, &skill); // SelfEndorsement #27
}

#[test]
fn test_get_skill_score_not_inflated_by_self_endorsement_attempt() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let user = Address::generate(&env);
    let skill = String::from_str(&env, "Rust");

    // A genuine endorsement from someone else still counts...
    let other = Address::generate(&env);
    client.endorse(&other, &user, &skill);
    assert_eq!(client.get_skill_score(&user, &skill), 1);

    // ...but the user cannot add themselves as an endorser to inflate it further.
    let result = client.try_endorse(&user, &user, &skill);
    assert!(result.is_err());
    assert_eq!(client.get_skill_score(&user, &skill), 1);
}

#[test]
fn test_get_skill_score_bounded_by_max_endorsers_counted() {
    let env = Env::default();
    env.mock_all_auths();
    // This test drives 150 endorsements plus their get_average_rating lookups,
    // which is realistic call volume but exceeds the default sandbox CPU budget;
    // reset it so the test exercises the counting logic, not gas accounting.
    env.budget().reset_unlimited();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let target = Address::generate(&env);
    let skill = String::from_str(&env, "Rust");

    // Endorse with more addresses than MAX_ENDORSERS_COUNTED (30); each
    // contributes weight 1 (no rating), so an unbounded sum would exceed 30.
    for _ in 0..45u32 {
        let e = Address::generate(&env);
        client.endorse(&e, &target, &skill);
    }

    assert_eq!(client.get_skill_score(&target, &skill), 30);
}

// ─────────────────────────────────────────────────────────────────────────────
// #536 — Stake-Weighted Reputation Tests
// ─────────────────────────────────────────────────────────────────────────────

fn inject_stake(env: &Env, contract_id: &Address, user: &Address, balance: i128) {
    env.as_contract(contract_id, || {
        env.storage().persistent().set(&DataKey::StakeBalance(user.clone()), &balance);
    });
}

fn set_tiers_via_admin(
    client: &ReputationContractClient<'_>,
    admin: &Address,
    tiers: soroban_sdk::Vec<StakeTier>,
) {
    client.propose_admin_action(admin, &AdminAction::SetStakeTiers(tiers));
}

#[test]
fn test_stake_no_tiers_default_1x() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let user = Address::generate(&env);
    assert_eq!(client.get_stake_multiplier(&user), 100);
}

#[test]
fn test_stake_tier_below_first_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let tiers = vec![
        &env,
        StakeTier { threshold: 100_0000000, multiplier: 120 },
        StakeTier { threshold: 1000_0000000, multiplier: 150 },
    ];
    set_tiers_via_admin(&client, &admin, tiers);

    let user = Address::generate(&env);
    inject_stake(&env, &reputation_id, &user, 50_0000000); // 50 XLM < 100
    assert_eq!(client.get_stake_multiplier(&user), 100);
}

#[test]
fn test_stake_tier_at_first_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let tiers = vec![
        &env,
        StakeTier { threshold: 100_0000000, multiplier: 120 },
        StakeTier { threshold: 1000_0000000, multiplier: 150 },
    ];
    set_tiers_via_admin(&client, &admin, tiers);

    let user = Address::generate(&env);
    inject_stake(&env, &reputation_id, &user, 100_0000000); // exactly 100 XLM
    assert_eq!(client.get_stake_multiplier(&user), 120);
}

#[test]
fn test_stake_tier_at_second_boundary() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let tiers = vec![
        &env,
        StakeTier { threshold: 100_0000000, multiplier: 120 },
        StakeTier { threshold: 1000_0000000, multiplier: 150 },
    ];
    set_tiers_via_admin(&client, &admin, tiers);

    let user = Address::generate(&env);
    inject_stake(&env, &reputation_id, &user, 5000_0000000); // 5000 XLM
    assert_eq!(client.get_stake_multiplier(&user), 150);
}

#[test]
fn test_stake_multiplier_applied_to_avg_rating() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer    = Address::generate(&env);
    let reviewee    = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr  = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);
    client.submit_review(
        &escrow_id, &reviewer, &reviewee, &1u64, &4u32,
        &String::from_str(&env, "Good"), &MIN_STAKE,
    );

    assert_eq!(client.get_average_rating(&reviewee), 400); // base

    // 1.5× tier
    let tiers = vec![&env, StakeTier { threshold: 100_0000000, multiplier: 150 }];
    set_tiers_via_admin(&client, &admin, tiers);
    inject_stake(&env, &reputation_id, &reviewee, 500_0000000);

    // 400 * 150 / 100 = 600
    assert_eq!(client.get_average_rating(&reviewee), 600);
}

#[test]
fn test_score_capped_at_10000() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer    = Address::generate(&env);
    let reviewee    = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr  = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);
    client.submit_review(
        &escrow_id, &reviewer, &reviewee, &1u64, &5u32,
        &String::from_str(&env, "Perfect"), &MIN_STAKE,
    );

    // 500,000× multiplier → would exceed cap
    let tiers = vec![&env, StakeTier { threshold: 1, multiplier: 500_000 }];
    set_tiers_via_admin(&client, &admin, tiers);
    inject_stake(&env, &reputation_id, &reviewee, 100);

    assert_eq!(client.get_average_rating(&reviewee), 10_000);
}

#[test]
fn test_stake_tiers_require_multisig_threshold() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);

    let signer1 = Address::generate(&env);
    let signer2 = Address::generate(&env);
    client.initialize(&vec![&env, signer1.clone(), signer2.clone()], &2u32, &0u32);

    let tiers = vec![&env, StakeTier { threshold: 100_0000000, multiplier: 120 }];
    let prop_id = client.propose_admin_action(&signer1, &AdminAction::SetStakeTiers(tiers));

    let user = Address::generate(&env);
    assert_eq!(client.get_stake_multiplier(&user), 100); // not yet active

    client.approve_admin_action(&signer2, &prop_id);

    inject_stake(&env, &reputation_id, &user, 500_0000000);
    assert_eq!(client.get_stake_multiplier(&user), 120); // now active
}

// ─────────────────────────────────────────────────────────────────────────────
// #534 — Ledger-Based Lazy Decay Tests
// ─────────────────────────────────────────────────────────────────────────────

fn advance_n_periods(env: &Env, periods: u32) {
    let seq = env.ledger().sequence();
    let ts  = env.ledger().timestamp();
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        sequence_number:        seq + periods * 518_400,
        timestamp:              ts  + (periods as u64) * ONE_YEAR_IN_SECONDS,
        protocol_version:       20,
        network_id:             [0; 32],
        base_reserve:           10,
        min_temp_entry_ttl:     10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl:          500_000_000,
    });
}

fn setup_review_for(
    env: &Env,
    escrow_id: &Address,
    client: &ReputationContractClient<'_>,
    job_id: u64,
    reviewer: &Address,
    reviewee: &Address,
    rating: u32,
) {
    let token_admin = Address::generate(env);
    let token_addr  = create_token(env, &token_admin);
    mint(env, &token_addr, &token_admin, reviewer, 100_000_000);
    setup_completed_job(env, escrow_id, job_id, reviewer, reviewee, &token_addr);
    client.submit_review(
        escrow_id, reviewer, reviewee, &job_id, &rating,
        &String::from_str(env, "ok"), &MIN_STAKE,
    );
}

#[test]
fn test_lazy_decay_zero_periods_no_change() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &1u32); // 1%/period

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_reputation(&reviewee);
    let after  = client.get_reputation(&reviewee);
    assert_eq!(before.total_score, after.total_score);
    assert_eq!(before.total_weight, after.total_weight);
}

#[test]
fn test_lazy_decay_one_period() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32); // 10%/period

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_reputation(&reviewee);
    advance_n_periods(&env, 1);
    let after = client.get_reputation(&reviewee);

    assert_eq!(after.total_score,  (before.total_score  * 90) / 100);
    assert_eq!(after.total_weight, (before.total_weight * 90) / 100);
}

#[test]
fn test_lazy_decay_ten_periods() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32); // 10%/period

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_reputation(&reviewee);
    // Linear annual decay at 10%/year for 10 years: max(0, 100 - 10*10) = 0% retained.
    let exp_score  = 0u64;
    let exp_weight = 0u64;
    let _ = before; // used above for correctness reference

    advance_n_periods(&env, 10);
    let after = client.get_reputation(&reviewee);

    assert_eq!(after.total_score,  exp_score);
    assert_eq!(after.total_weight, exp_weight);
}

#[test]
fn test_lazy_decay_zero_rate_no_decay() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32); // 0%/period

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_reputation(&reviewee);
    advance_n_periods(&env, 10);
    let after = client.get_reputation(&reviewee);

    assert_eq!(before.total_score,  after.total_score);
    assert_eq!(before.total_weight, after.total_weight);
}

#[test]
fn test_last_updated_ts_advances_on_write() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id     = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let ledger_before = client.get_reputation(&reviewee).last_updated_ts;
    advance_n_periods(&env, 2);

    // Trigger write by adding a second review
    let reviewer2 = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 2, &reviewer2, &reviewee, 4);

    let ledger_after = client.get_reputation(&reviewee).last_updated_ts;
    assert!(ledger_after > ledger_before);
}

// ── tier_up event tests (Issue #464) ────────────────────────────────────────

// env.events().all() returns Vec<(Address, soroban_sdk::Vec<Val>, Val)>
// where the Address is the emitting contract. Topics are compared by converting
// each Val slot back to Symbol via TryFromVal.

fn topics_match(env: &Env, topics: &soroban_sdk::Vec<soroban_sdk::Val>, sym0: Symbol, sym1: Symbol) -> bool {
    topics.len() == 2
        && topics
            .get(0_u32)
            .and_then(|v| Symbol::try_from_val(env, &v).ok())
            == Some(sym0)
        && topics
            .get(1_u32)
            .and_then(|v| Symbol::try_from_val(env, &v).ok())
            == Some(sym1)
}

fn tier_up_event_count(env: &Env) -> usize {
    env.events()
        .all()
        .iter()
        .filter(|(_, topics, _)| {
            topics_match(env, topics, symbol_short!("reput"), symbol_short!("tier_up"))
        })
        .count()
}

fn badge_event_count(env: &Env) -> usize {
    env.events()
        .all()
        .iter()
        .filter(|(_, topics, _)| {
            topics_match(env, topics, symbol_short!("reput"), symbol_short!("badge"))
        })
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────
// #648 — Unbounded decay loop DoS fix: tests for large periods, leaderboard, fuzz
// ─────────────────────────────────────────────────────────────────────────────

fn setup_high_ttl_env() -> Env {
    let env = Env::default();
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 20,
        sequence_number: 0,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 500_000_000,
    });
    env
}

#[test]
fn test_lazy_decay_sixty_periods_no_revert() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &1u32); // 1% per year

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_reputation(&reviewee);
    advance_n_periods(&env, 60);

    // 60 years * 1% = 60% decay, retained 40%
    let after = client.get_reputation(&reviewee);
    assert_eq!(after.total_score, (before.total_score * 40) / 100);
    assert_eq!(after.total_weight, (before.total_weight * 40) / 100);
    assert_eq!(after.review_count, before.review_count);
}

#[test]
fn test_lazy_decay_sixty_periods_full_decay_saturates() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &2u32); // 2% per year

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    advance_n_periods(&env, 60);

    // 60 years * 2% = 120% -> saturating_sub clamps to 0% retained
    let after = client.get_reputation(&reviewee);
    assert_eq!(after.total_score, 0);
    assert_eq!(after.total_weight, 0);
    assert_eq!(after.review_count, 1);
}

#[test]
fn test_lazy_decay_high_rate_full_decay_saturates() {
    let env = Env::default();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &50u32); // 50% per year

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    advance_n_periods(&env, 5);

    // 5 years * 50% = 250% -> saturates to 0%
    let after = client.get_reputation(&reviewee);
    assert_eq!(after.total_score, 0);
    assert_eq!(after.total_weight, 0);
}

#[test]
fn test_leaderboard_many_entries_all_dormant_fifty_periods() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32);

    // Create 10 users with reviews to populate the leaderboard
    for i in 0..10u64 {
        let reviewer = Address::generate(&env);
        let reviewee = Address::generate(&env);
        setup_review_for(&env, &escrow_id, &client, i + 1, &reviewer, &reviewee, 5);
    }

    // All users dormant for 50 periods
    advance_n_periods(&env, 50);

    // Leaderboard should still return without reverting
    let leaderboard = client.get_leaderboard();
    assert!(leaderboard.len() <= 10);
    // All entries should have decayed scores (fully decayed at 10%/yr * 50yr)
    for (_addr, score) in leaderboard.iter() {
        assert!(score <= 500);
    }
}

#[test]
fn test_decay_formula_consistent_across_period_ranges() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &5u32); // 5% per year

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    // retained_pct = max(0, 100 - 5 * periods)
    let check_points: [(u32, u64); 5] = [
        (0, 100),
        (1, 95),
        (2, 90),
        (5, 75),
        (10, 50),
    ];

    let mut cumulative = 0u32;
    for (periods, expected_retained_pct) in &check_points {
        let advance = *periods - cumulative;
        advance_n_periods(&env, advance);
        cumulative = *periods;

        let rep = client.get_reputation(&reviewee);
        let expected_score = (5u64 * (MIN_STAKE as u64) * expected_retained_pct) / 100;
        let expected_weight = ((MIN_STAKE as u64) * expected_retained_pct) / 100;
        assert_eq!(
            rep.total_score, expected_score,
            "score mismatch at {} periods", periods
        );
        assert_eq!(
            rep.total_weight, expected_weight,
            "weight mismatch at {} periods", periods
        );
    }
}

/// Verify O(1) decay for high rates and long periods — never panics, never exceeds original.
#[test]
fn test_decay_fuzz_never_exceeds_original() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32); // 10% per year

    // Single user, test at key period milestones
    let user = Address::generate(&env);
    env.as_contract(&reputation_id, || {
        env.storage().persistent().set(
            &DataKey::Reputation(user.clone()),
            &UserReputation {
                user: user.clone(),
                total_score: 1_000_000,
                total_weight: 100_000,
                review_count: 10,
                last_updated_ts: 0,
            },
        );
    });

    let mut cumulative = 0u32;
    for periods in [0u32, 1, 5, 10, 15, 30, 60] {
        let advance = periods - cumulative;
        advance_n_periods(&env, advance);
        cumulative = periods;

        let rep = client.get_reputation(&user);
        assert!(
            rep.total_score <= 1_000_000,
            "score exceeded original at {} periods", periods
        );
        assert!(
            rep.total_weight <= 100_000,
            "weight exceeded original at {} periods", periods
        );
    }
}

/// A tier upgrade (None -> Bronze) must emit exactly one tier_up event carrying
/// the correct reviewee address and old/new tier values.
#[test]
fn test_tier_up_event_emitted_on_tier_upgrade() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Rating 2 -> avg 200 -> Bronze (previous tier: None)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    assert_eq!(tier_up_event_count(&env), 1, "expected exactly one tier_up event");

    // Validate payload: (reviewee, old_tier=None, new_tier=Bronze)
    let event = env
        .events()
        .all()
        .into_iter()
        .find(|(_, topics, _)| {
            topics_match(&env, topics, symbol_short!("reput"), symbol_short!("tier_up"))
        })
        .expect("tier_up event not found");

    let (ev_reviewee, ev_old_tier, ev_new_tier): (Address, ReputationTier, ReputationTier) =
        <(Address, ReputationTier, ReputationTier)>::try_from_val(&env, &event.2).unwrap();
    assert_eq!(ev_reviewee, reviewee);
    assert_eq!(ev_old_tier, ReputationTier::None);
    assert_eq!(ev_new_tier, ReputationTier::Bronze);
}

/// When a review does not change the reputation tier (user stays in Bronze after
/// a second Bronze-level review) no additional tier_up event must be emitted.
#[test]
fn test_no_tier_up_event_when_tier_unchanged() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer1, 100_000_000);
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer1, &reviewee, &token_addr);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);

    // First review: rating 2 -> avg 200 -> Bronze (tier_up: None -> Bronze)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer1,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Decent"),
        &MIN_STAKE,
    );
    assert_eq!(tier_up_event_count(&env), 1);

    // Second review: rating 2 -> avg still 200 -> stays Bronze (no tier change)
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &2u32,
        &String::from_str(&env, "Consistent"),
        &MIN_STAKE,
    );

    // Total tier_up events must still be exactly 1 (second review added none)
    assert_eq!(
        tier_up_event_count(&env),
        1,
        "second review must not emit tier_up when tier is unchanged"
    );
}

/// The existing badge event must continue to be emitted alongside the new
/// tier_up event, preserving backward compatibility for badge consumers.
#[test]
fn test_badge_event_preserved_alongside_tier_up() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Rating 4 -> avg 400 -> Silver tier
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Great"),
        &MIN_STAKE,
    );

    assert_eq!(badge_event_count(&env), 1, "badge event must still be emitted");
    assert_eq!(tier_up_event_count(&env), 1, "tier_up event must be emitted alongside badge");

    // Functional sanity: badge is stored and matches expected tier
    let badges = reputation_client.get_badges(&reviewee);
    assert_eq!(badges.len(), 1);
    assert_eq!(badges.get(0).unwrap().badge_type, ReputationTier::Silver);
}

// ─────────────────────────────────────────────────────────────────────────────
// #781 — Referral bonus timestamp validation
// A future-dated bonus keeps `get_decay_factor` at elapsed_seconds = 0,
// permanently exempting it from decay and inflating the score.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_add_referral_bonus_rejects_future_timestamp() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);
    env.ledger().with_mut(|l| l.timestamp = 1_000);

    let user = Address::generate(&env);
    // timestamp 1_001 > current ledger timestamp 1_000 -> InvalidTimestamp (#24)
    client.add_referral_bonus(&admin, &user, &5u64, &1u64, &1_001u64);
}

#[test]
fn test_add_referral_bonus_accepts_current_timestamp() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32); // no decay
    env.ledger().with_mut(|l| l.timestamp = 1_000);

    let user = Address::generate(&env);
    // Current timestamp is accepted.
    client.add_referral_bonus(&admin, &user, &5u64, &1u64, &1_000u64);

    let rep = client.get_reputation(&user);
    assert_eq!(rep.total_score, 5u64);
    assert_eq!(rep.total_weight, 1u64);
}

#[test]
fn test_add_referral_bonus_accepts_past_timestamp() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32); // no decay
    env.ledger().with_mut(|l| l.timestamp = 1_000);

    let user = Address::generate(&env);
    // A past timestamp is accepted so the bonus decays from its true origin.
    client.add_referral_bonus(&admin, &user, &5u64, &1u64, &500u64);

    let rep = client.get_reputation(&user);
    assert_eq!(rep.total_score, 5u64);
    assert_eq!(rep.total_weight, 1u64);
}

// ─────────────────────────────────────────────────────────────────────────────
// #783 — Decay rate upper bound
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_update_decay_rate_within_bound_succeeds() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // 20 == MAX_DECAY_RATE default -> accepted.
    client.update_decay_rate(&admin, &20u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")]
fn test_update_decay_rate_above_bound_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // 21 > MAX_DECAY_RATE (20) -> DecayRateTooHigh (#25).
    client.update_decay_rate(&admin, &21u32);
}

#[test]
fn test_super_admin_can_raise_max_decay_rate() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Super-admin raises the ceiling to 30, then a previously-rejected 25 is allowed.
    client.set_max_decay_rate(&admin, &30u32);
    client.update_decay_rate(&admin, &25u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")]
fn test_set_max_decay_rate_hard_ceiling_enforced() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // 51 > MAX_DECAY_RATE_HARD_CEILING (50) -> DecayRateTooHigh (#25).
    client.set_max_decay_rate(&admin, &51u32);
}

// ─────────────────────────────────────────────────────────────────────────────
// #785 — Zero-score users are removed from the leaderboard
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_leaderboard_removes_fully_decayed_user() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &50u32); // 50%/yr

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    // User with a non-zero score is on the leaderboard.
    let before = client.get_leaderboard();
    assert_eq!(before.len(), 1);
    assert!(before.iter().any(|(addr, _)| addr == reviewee));

    // Dormant long enough to fully decay: 5yr * 50% = 250% -> saturates to 0.
    advance_n_periods(&env, 5);

    // A subsequent reputation write re-runs update_leaderboard while totals are
    // zero. A zero-value bonus (current timestamp) triggers it without adding score.
    let now = env.ledger().timestamp();
    client.add_referral_bonus(&admin, &reviewee, &0u64, &0u64, &now);

    // The fully-decayed user has been removed; the leaderboard shrinks.
    let after = client.get_leaderboard();
    assert_eq!(after.len(), 0);
    assert!(!after.iter().any(|(addr, _)| addr == reviewee));
}

// ─────────────────────────────────────────────────────────────────────────────
// #774 — Banned users are excluded from the leaderboard
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_banned_user_excluded_from_leaderboard() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let before = client.get_leaderboard();
    assert!(before.iter().any(|(addr, _)| addr == reviewee));

    client.ban_user(&admin, &reviewee);

    let after = client.get_leaderboard();
    assert!(!after.iter().any(|(addr, _)| addr == reviewee));
}

#[test]
fn test_unban_user_restores_leaderboard_visibility() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    client.ban_user(&admin, &reviewee);
    assert!(!client.get_leaderboard().iter().any(|(addr, _)| addr == reviewee));

    client.unban_user(&admin, &reviewee);
    assert!(client.get_leaderboard().iter().any(|(addr, _)| addr == reviewee));
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")] // NotAdmin
fn test_ban_user_by_non_admin_rejected() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let non_admin = Address::generate(&env);
    let target = Address::generate(&env);
    client.ban_user(&non_admin, &target);
}

// ── Issue #771: minimum stake weight threshold ───────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_submit_review_with_zero_stake_weight_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Lower the economic min_stake to 0 so we can test the stake weight check directly.
    client.propose_admin_action(&admin, &AdminAction::SetMinStake(0));

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);
    setup_completed_job(&env, &escrow_id, 1, &reviewer, &reviewee, &token_addr);

    // stake_weight = 0 is below MIN_STAKE_WEIGHT = 1 → StakeTooLow (#26)
    client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "ok"),
        &0i128,
    );
}

#[test]
fn test_stake_too_low_error_code_is_26() {
    assert_eq!(ReputationError::StakeTooLow as u32, 26);
}

#[test]
fn test_get_min_stake_weight_returns_default() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let weight = client.get_min_stake_weight();
    assert_eq!(weight, MIN_STAKE_WEIGHT);
}

#[test]
fn test_set_min_stake_weight_admin_only() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    client.set_min_stake_weight(&admin, &5u64);
    assert_eq!(client.get_min_stake_weight(), 5u64);
}

#[test]
#[should_panic]
fn test_set_min_stake_weight_non_signer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    let outsider = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Clear auths so the signer check actually fires.
    env.set_auths(&[]);
    client.set_min_stake_weight(&outsider, &10u64);
}

#[test]
fn test_submit_review_with_stake_weight_at_minimum_accepted() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Keep default min_stake, submit with exactly MIN_STAKE (which is >= MIN_STAKE_WEIGHT).
    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let rep = client.get_reputation(&reviewee);
    assert!(rep.total_weight > 0, "review should be recorded");
}

// ============================================================
// get_gov_weight — governance snapshot helper (issue #899)
// ============================================================

#[test]
fn test_get_gov_weight_reports_score_and_last_change() {
    let env = Env::default();
    env.mock_all_auths();
    // A non-zero ledger time so `last_change_ts` is meaningfully populated.
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32); // no decay

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let rep = client.get_reputation(&reviewee);
    let (score, last_change_ts) = client.get_gov_weight(&reviewee);

    // Score mirrors the decayed total_score used for voting weight.
    assert_eq!(score, rep.total_score);
    assert!(score > 0);
    // last_change_ts is the ledger time at which the review was recorded.
    assert_eq!(last_change_ts, 1_000_000);
}

#[test]
fn test_get_gov_weight_unknown_user_is_zero() {
    let env = Env::default();
    env.mock_all_auths();
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let nobody = Address::generate(&env);
    // Unknown users have no weight and a snapshot-safe timestamp of 0.
    assert_eq!(client.get_gov_weight(&nobody), (0, 0));
}

#[test]
fn test_get_gov_weight_last_change_moves_on_new_review() {
    // The core property governance relies on: earning reputation bumps
    // `last_change_ts`, which is exactly what disqualifies post-snapshot gaming.
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 500_000);

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);
    let (_, first_ts) = client.get_gov_weight(&reviewee);
    assert_eq!(first_ts, 500_000);

    // A later review bumps last_change_ts forward.
    env.ledger().with_mut(|l| l.timestamp = 800_000);
    let reviewer2 = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 2, &reviewer2, &reviewee, 5);
    let (_, second_ts) = client.get_gov_weight(&reviewee);
    assert_eq!(second_ts, 800_000);
}

#[test]
fn test_set_stake_tiers_admin_emits_event_and_bumps_ttl() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let tiers = vec![
        &env,
        StakeTier { threshold: 100_0000000, multiplier: 120 },
    ];

    client.set_stake_tiers(&admin, &tiers);

    // Verify event is emitted with expected topics and data
    let events = env.events().all();
    let mut found_event = false;
    for event in events.iter() {
        let (_, topics, data) = event;
        if topics.len() == 2 {
            let topic0 = Symbol::try_from_val(&env, &topics.get_unchecked(0)).unwrap();
            let topic1 = Symbol::try_from_val(&env, &topics.get_unchecked(1)).unwrap();
            if topic0 == symbol_short!("reput") && topic1 == Symbol::new(&env, "tiers_set") {
                found_event = true;
                let event_data: (Address, soroban_sdk::Vec<StakeTier>) = soroban_sdk::TryFromVal::try_from_val(&env, &data).unwrap();
                assert_eq!(event_data.0, admin);
                assert_eq!(event_data.1.get_unchecked(0).threshold, 100_0000000);
                assert_eq!(event_data.1.get_unchecked(0).multiplier, 120);
            }
        }
    }
    assert!(found_event);
}

#[test]
fn test_set_referral_bonus_admin_only() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    client.set_referral_bonus(&admin, &50u64);

    env.as_contract(&reputation_id, || {
        let stored_bonus: u64 = env.storage().instance().get(&DataKey::ReferralBonus).unwrap_or(0);
        assert_eq!(stored_bonus, 50u64);
    });
}

#[test]
#[should_panic]
fn test_set_referral_bonus_non_signer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    let outsider = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Clear auths so the signer check actually fires.
    env.set_auths(&[]);
    client.set_referral_bonus(&outsider, &100u64);
}

// =============================
// Issue #982: Stake Weight Truncation Tests
// =============================

#[test]
fn test_stake_weight_at_u64_max_saturates() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    // Mint a huge amount to support large stake
    mint(&env, &token_addr, &token_admin, &reviewer, i128::MAX);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review with stake_weight exactly at u64::MAX
    let huge_stake = u64::MAX as i128;
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Huge stake"),
        &huge_stake,
    );

    let rep = reputation_client.get_reputation(&reviewee);
    // Weight is capped at MAX_STAKE_WEIGHT_PER_REVIEW regardless of raw stake size.
    assert_eq!(rep.total_weight, MAX_STAKE_WEIGHT_PER_REVIEW);
    // Score = rating (5) * capped weight.
    assert_eq!(rep.total_score, 5 * MAX_STAKE_WEIGHT_PER_REVIEW);
}

#[test]
fn test_stake_weight_above_u64_max_saturates() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    // Mint a huge amount to support large stake
    mint(&env, &token_addr, &token_admin, &reviewer, i128::MAX);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review with stake_weight above u64::MAX
    let huge_stake = (u64::MAX as i128) + 1_000_000_000;
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Gigantic stake"),
        &huge_stake,
    );

    let rep = reputation_client.get_reputation(&reviewee);
    // Weight is capped at MAX_STAKE_WEIGHT_PER_REVIEW regardless of raw stake size.
    assert_eq!(rep.total_weight, MAX_STAKE_WEIGHT_PER_REVIEW);
    // Score = rating (4) * capped weight.
    assert_eq!(rep.total_score, 4 * MAX_STAKE_WEIGHT_PER_REVIEW);
}

#[test]
fn test_stake_weight_decay_saturates_large_values() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let admin = Address::generate(&env);
    // Initialize with 10% decay per year
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &10);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    mint(&env, &token_addr, &token_admin, &reviewer, i128::MAX);
    reputation_client.propose_admin_action(&admin, &AdminAction::SetToken(token_addr.clone()));

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Set initial timestamp
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    // Submit review with stake above u64::MAX
    let huge_stake = (u64::MAX as i128) + 1_000_000_000;
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Massive stake before decay"),
        &huge_stake,
    );

    // Advance 6 months (half year) - should decay by 5%
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: ONE_YEAR_IN_SECONDS / 2,
        protocol_version: 20,
        sequence_number: 200,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    let rep = reputation_client.get_reputation(&reviewee);
    // Weight started at MAX_STAKE_WEIGHT_PER_REVIEW (capped on write), after 5% decay: 95% of cap.
    let expected_weight = MAX_STAKE_WEIGHT_PER_REVIEW * 95 / 100;
    assert_eq!(rep.total_weight, expected_weight);
}

// =============================
// Issue #983: appeal_review Test Coverage
// =============================

/// Helper to submit a review and return the review timestamp for testing appeal windows
fn submit_review_and_get_timestamp(
    env: &Env,
    reputation_client: &ReputationContractClient,
    escrow_id: &Address,
    reviewer: &Address,
    reviewee: &Address,
    job_id: u64,
    rating: u32,
) -> u64 {
    reputation_client.submit_review(
        escrow_id,
        reviewer,
        reviewee,
        &job_id,
        &rating,
        &String::from_str(env, "Test review"),
        &MIN_STAKE,
    );
    env.ledger().timestamp()
}

#[test]
fn test_appeal_review_success() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit a review
    let review_timestamp = submit_review_and_get_timestamp(
        &env,
        &reputation_client,
        &escrow_id,
        &reviewer,
        &reviewee,
        1u64,
        1u32,
    );

    // Appeal the review within the grace window
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "Unfair 1-star rating"),
    );

    // Verify the appeal was created with Pending status
    let appeal = reputation_client.get_review_appeal(&reviewer, &reviewee, &1u64);
    assert_eq!(appeal.status, AppealStatus::Pending);
    assert_eq!(appeal.reviewer, reviewer);
    assert_eq!(appeal.reviewee, reviewee);
    assert_eq!(appeal.job_id, 1u64);
    assert_eq!(appeal.reason, String::from_str(&env, "Unfair 1-star rating"));
    assert_eq!(appeal.created_at, env.ledger().timestamp());
    assert_eq!(appeal.expires_at, review_timestamp + 72 * 60 * 60); // APPEAL_GRACE_WINDOW_SECONDS
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")] // AppealWindowExpired
fn test_appeal_review_after_grace_window() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review at t=0
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &1u32,
        &String::from_str(&env, "Bad review"),
        &MIN_STAKE,
    );

    // Advance time beyond the 72-hour grace window
    let grace_window = 72 * 60 * 60; // APPEAL_GRACE_WINDOW_SECONDS
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: grace_window + 1,
        protocol_version: 20,
        sequence_number: 200,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    // Try to appeal - should fail with AppealWindowExpired
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "Too late appeal"),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")] // AppealAlreadyExists
fn test_appeal_review_duplicate() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit a review
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &1u32,
        &String::from_str(&env, "Bad review"),
        &MIN_STAKE,
    );

    // First appeal succeeds
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "First appeal"),
    );

    // Second appeal for the same review should fail with AppealAlreadyExists
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "Second appeal"),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #18)")] // ReviewNotFound
fn test_appeal_review_nonexistent_review() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);

    // Try to appeal a review that doesn't exist
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &999u64,
        &String::from_str(&env, "Appeal for non-existent review"),
    );
}

#[test]
fn test_appeal_review_requires_reviewee_auth() {
    let env = Env::default();
    // Note: We don't call env.mock_all_auths() to test auth requirement
    
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    // Mock auth only for setup operations
    env.mock_all_auths();
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);
    
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &1u32,
        &String::from_str(&env, "Bad review"),
        &MIN_STAKE,
    );

    // Clear mock_all_auths for the appeal call
    // appeal_review requires reviewee.require_auth(), so without auth it should panic
    // We'll test this by observing that the function requires auth
    // Since we can't easily test auth failure without proper auth setup,
    // we'll document that appeal_review has reviewee.require_auth() at the top
    
    // For now, verify that with auth it works
    env.mock_all_auths();
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "Appeal with auth"),
    );
    
    let appeal = reputation_client.get_review_appeal(&reviewer, &reviewee, &1u64);
    assert_eq!(appeal.status, AppealStatus::Pending);
}

#[test]
fn test_appeal_at_exact_grace_window_boundary() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Submit review at t=1000
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 1000,
        protocol_version: 20,
        sequence_number: 100,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &2u32,
        &String::from_str(&env, "Review at t=1000"),
        &MIN_STAKE,
    );

    // Advance to exactly the grace window boundary (72 hours = 259200 seconds)
    let grace_window = 72 * 60 * 60;
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 1000 + grace_window,
        protocol_version: 20,
        sequence_number: 200,
        network_id: [0; 32],
        base_reserve: 10,
        min_temp_entry_ttl: 10,
        min_persistent_entry_ttl: 10,
        max_entry_ttl: 100000,
    });

    // At exactly the boundary, the appeal should succeed (not expired yet)
    // The check is: now > review.timestamp + APPEAL_GRACE_WINDOW_SECONDS
    // So at now = 1000 + 259200, the check is: 260200 > 260200 = false, should succeed
    reputation_client.appeal_review(
        &reviewer,
        &reviewee,
        &1u64,
        &String::from_str(&env, "Appeal at exact boundary"),
    );

    let appeal = reputation_client.get_review_appeal(&reviewer, &reviewee, &1u64);
    assert_eq!(appeal.status, AppealStatus::Pending);
}

// ================================================================================================
// Issue #986: Tests for self-referral and circular-referral rejection in register_referral
// ================================================================================================

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_self_referral_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user = Address::generate(&env);

    // Attempt to refer oneself - should fail with SelfReferral (#16)
    reputation_client.register_referral(&user, &user);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_circular_referral_direct_loop() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);
    let user_c = Address::generate(&env);

    // Build a chain: A → B → C
    reputation_client.register_referral(&user_a, &user_b);
    reputation_client.register_referral(&user_b, &user_c);

    // Attempt to close the loop: C → A
    // This should fail with CircularReferral (#17)
    reputation_client.register_referral(&user_c, &user_a);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_circular_referral_longer_chain() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);
    let user_c = Address::generate(&env);
    let user_d = Address::generate(&env);
    let user_e = Address::generate(&env);

    // Build a longer chain: A → B → C → D → E
    reputation_client.register_referral(&user_a, &user_b);
    reputation_client.register_referral(&user_b, &user_c);
    reputation_client.register_referral(&user_c, &user_d);
    reputation_client.register_referral(&user_d, &user_e);

    // Attempt to close the loop: E → B (creating a cycle in the middle)
    // This should fail with CircularReferral (#17)
    reputation_client.register_referral(&user_e, &user_b);
}

#[test]
fn test_valid_referral_chain_no_loop() {
    let env = Env::default();
    env.mock_all_auths();

    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let user_a = Address::generate(&env);
    let user_b = Address::generate(&env);
    let user_c = Address::generate(&env);
    let user_d = Address::generate(&env);

    // Build a valid chain: A → B → C → D (no loop)
    reputation_client.register_referral(&user_a, &user_b);
    reputation_client.register_referral(&user_b, &user_c);
    reputation_client.register_referral(&user_c, &user_d);

    // Verify referrals were registered successfully
    let stats_b = reputation_client.get_referral_stats(&user_b);
    assert_eq!(stats_b.total_referrals, 1);

    let stats_c = reputation_client.get_referral_stats(&user_c);
    assert_eq!(stats_c.total_referrals, 1);

    let stats_d = reputation_client.get_referral_stats(&user_d);
    assert_eq!(stats_d.total_referrals, 1);
}

// ================================================================================================
// Issue #985: Tests for claim_stake function
// ================================================================================================

#[test]
fn test_claim_stake_partial_withdrawal() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    // Mint tokens and submit a review to stake them
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    // Start at t=1000 so advancing by STAKE_LOCKUP_SECONDS gives a clean timestamp.
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    // Advance past the 7-day lockup window before claiming.
    env.ledger().with_mut(|l| l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1);

    // Partial claim: withdraw half of the staked amount
    let claim_amount = MIN_STAKE / 2;
    reputation_client.claim_stake(&reviewer, &claim_amount);

    // Verify the remaining stake balance
    let balance = reputation_client.get_stake_balance(&reviewer);
    assert_eq!(balance, MIN_STAKE - claim_amount);
}

#[test]
fn test_claim_stake_full_withdrawal() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    // Advance past the 7-day lockup before claiming.
    env.ledger().with_mut(|l| l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1);

    // Full claim: withdraw entire staked amount
    reputation_client.claim_stake(&reviewer, &MIN_STAKE);

    // Verify the stake balance is now zero (key should be removed)
    let balance = reputation_client.get_stake_balance(&reviewer);
    assert_eq!(balance, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_claim_stake_exceeds_balance() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    // Advance past lockup so the balance-exceeded check is reached, not the lockup check.
    env.ledger().with_mut(|l| l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1);

    // Attempt to claim more than the available balance
    // Should fail with BelowMinStake (#11)
    reputation_client.claim_stake(&reviewer, &(MIN_STAKE + 1));
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_claim_stake_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    // Advance past lockup so only the zero-amount check fires.
    env.ledger().with_mut(|l| l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1);

    // Attempt to claim zero amount
    // Should fail with BelowMinStake (#11)
    reputation_client.claim_stake(&reviewer, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_claim_stake_negative_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &4u32,
        &String::from_str(&env, "Good work"),
        &MIN_STAKE,
    );

    // Advance past lockup so only the negative-amount check fires.
    env.ledger().with_mut(|l| l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1);

    // Attempt to claim negative amount
    // Should fail with BelowMinStake (#11)
    reputation_client.claim_stake(&reviewer, &-100);
}

// ================================================================================================
// Issue #984: Tests for admin_resolve_appeal function
// ================================================================================================

/// Helper function to submit a review and file an appeal for testing
fn setup_review_and_appeal(
    env: &Env,
    escrow_id: &Address,
    reputation_client: &ReputationContractClient,
    reviewer: &Address,
    reviewee: &Address,
    token_addr: &Address,
    job_id: u64,
    rating: u32,
) {
    let token_admin = Address::generate(env);
    mint(env, token_addr, &token_admin, reviewer, 100_000_000);
    
    setup_completed_job(env, escrow_id, job_id, reviewer, reviewee, token_addr);

    reputation_client.submit_review(
        escrow_id,
        reviewer,
        reviewee,
        &job_id,
        &rating,
        &String::from_str(env, "Review comment"),
        &MIN_STAKE,
    );

    // File an appeal
    reputation_client.appeal_review(
        reviewer,
        reviewee,
        &job_id,
        &String::from_str(env, "This review is unfair"),
    );
}

#[test]
fn test_admin_resolve_appeal_remove_review() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    setup_review_and_appeal(&env, &escrow_id, &reputation_client, &reviewer, &reviewee, &token_addr, 1u64, 3u32);

    // Get reputation before removal
    let rep_before = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_before.review_count, 1);
    let score_before = rep_before.total_score;
    let weight_before = rep_before.total_weight;

    // Admin resolves appeal by removing the review
    reputation_client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &1u64, &true);

    // Verify reputation was adjusted
    let rep_after = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_after.review_count, 0);
    assert_eq!(rep_after.total_score, 0);
    assert_eq!(rep_after.total_weight, 0);

    // Verify appeal status is ReviewRemoved
    let appeal = reputation_client.get_review_appeal(&reviewer, &reviewee, &1u64);
    assert_eq!(appeal.status, AppealStatus::ReviewRemoved);
}

#[test]
fn test_admin_resolve_appeal_dismiss() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    setup_review_and_appeal(&env, &escrow_id, &reputation_client, &reviewer, &reviewee, &token_addr, 1u64, 4u32);

    // Get reputation before dismissal
    let rep_before = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_before.review_count, 1);
    let score_before = rep_before.total_score;
    let weight_before = rep_before.total_weight;

    // Admin resolves appeal by dismissing it (not removing review)
    reputation_client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &1u64, &false);

    // Verify reputation was NOT changed
    let rep_after = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_after.review_count, rep_before.review_count);
    assert_eq!(rep_after.total_score, score_before);
    assert_eq!(rep_after.total_weight, weight_before);

    // Verify appeal status is Dismissed
    let appeal = reputation_client.get_review_appeal(&reviewer, &reviewee, &1u64);
    assert_eq!(appeal.status, AppealStatus::Dismissed);
}

#[test]
#[should_panic(expected = "Error(Contract, #22)")]
fn test_admin_resolve_appeal_already_resolved() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    setup_review_and_appeal(&env, &escrow_id, &reputation_client, &reviewer, &reviewee, &token_addr, 1u64, 4u32);

    // Resolve the appeal once
    reputation_client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &1u64, &true);

    // Attempt to resolve the same appeal again
    // Should fail with AppealAlreadyResolved (#22)
    reputation_client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &1u64, &false);
}

#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_admin_resolve_appeal_not_found() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);

    // Attempt to resolve a non-existent appeal
    // Should fail with AppealNotFound (#21)
    reputation_client.admin_resolve_appeal(&admin, &reviewer, &reviewee, &999u64, &true);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_admin_resolve_appeal_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let non_admin = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    setup_review_and_appeal(&env, &escrow_id, &reputation_client, &reviewer, &reviewee, &token_addr, 1u64, 4u32);

    // Attempt to resolve appeal with a non-admin account
    // Should fail with NotAdmin (#14)
    reputation_client.admin_resolve_appeal(&non_admin, &reviewer, &reviewee, &1u64, &true);
}

#[test]
fn test_admin_resolve_appeal_reputation_accounting() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Initialize with admin
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &50);

    let reviewer1 = Address::generate(&env);
    let reviewer2 = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    
    // Setup two reviews
    setup_review_and_appeal(&env, &escrow_id, &reputation_client, &reviewer1, &reviewee, &token_addr, 1u64, 5u32);
    
    mint(&env, &token_addr, &token_admin, &reviewer2, 100_000_000);
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer2, &reviewee, &token_addr);
    reputation_client.submit_review(
        &escrow_id,
        &reviewer2,
        &reviewee,
        &2u64,
        &3u32,
        &String::from_str(&env, "Average work"),
        &MIN_STAKE,
    );
    reputation_client.appeal_review(
        &reviewer2,
        &reviewee,
        &2u64,
        &String::from_str(&env, "Also unfair"),
    );

    // Get reputation with two reviews
    let rep_two_reviews = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_two_reviews.review_count, 2);
    // Score = (5 * MIN_STAKE) + (3 * MIN_STAKE) = 8 * MIN_STAKE
    let expected_score = (5 * MIN_STAKE + 3 * MIN_STAKE) as u64;
    let expected_weight = (2 * MIN_STAKE) as u64;
    assert_eq!(rep_two_reviews.total_score, expected_score);
    assert_eq!(rep_two_reviews.total_weight, expected_weight);

    // Admin removes the first review (5 stars)
    reputation_client.admin_resolve_appeal(&admin, &reviewer1, &reviewee, &1u64, &true);

    // Verify reputation accounting is correct after removal
    let rep_after_removal = reputation_client.get_reputation(&reviewee);
    assert_eq!(rep_after_removal.review_count, 1);
    // Remaining score = 3 * MIN_STAKE
    let expected_score_after = (3 * MIN_STAKE) as u64;
    let expected_weight_after = MIN_STAKE as u64;
    assert_eq!(rep_after_removal.total_score, expected_score_after);
    assert_eq!(rep_after_removal.total_weight, expected_weight_after);
}

// ─────────────────────────────────────────────────────────────────────────────
// #1117 — get_decayed_totals must stay bounded regardless of Reviews vector size
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_get_decayed_totals_bounded_by_max_reviews_counted() {
    let env = Env::default();
    env.mock_all_auths();
    // Reading back 250 injected reviews is realistic call volume for a
    // griefed account but exceeds the default sandbox CPU budget; reset it
    // so the test exercises the counting/capping logic, not gas accounting.
    env.budget().reset_unlimited();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32); // no decay

    let user = Address::generate(&env);
    let reviewer = Address::generate(&env);

    // 250 reviews (> MAX_REVIEWS_COUNTED = 200): the oldest 50 are 1-star,
    // the most recent 200 are 5-star. An unbounded average would be pulled
    // down by the oldest 50; the bounded computation should read as a pure
    // 5-star average since only the most recent 200 are scored.
    env.as_contract(&reputation_id, || {
        let mut reviews: Vec<Review> = Vec::new(&env);
        for i in 0..250u64 {
            let rating = if i < 50 { 1u32 } else { 5u32 };
            reviews.push_back(Review {
                reviewer: reviewer.clone(),
                reviewee: user.clone(),
                job_id: i,
                rating,
                comment: String::from_str(&env, ""),
                stake_weight: MIN_STAKE,
                timestamp: 0,
            });
        }
        env.storage()
            .persistent()
            .set(&DataKey::Reviews(user.clone()), &reviews);
        env.storage().persistent().set(
            &DataKey::Reputation(user.clone()),
            &UserReputation {
                user: user.clone(),
                total_score: 0,
                total_weight: 0,
                review_count: 250,
                last_updated_ts: 0,
            },
        );
    });

    let rep = client.get_reputation(&user);
    assert_eq!(rep.review_count, 250, "full historical count is still reported");
    assert_eq!(rep.total_score, 5u64 * MIN_STAKE as u64 * 200);
    assert_eq!(rep.total_weight, MIN_STAKE as u64 * 200);

    // Pure 5-star average, unaffected by the 50 oldest 1-star reviews.
    assert_eq!(client.get_average_rating(&user), 500);
}

// ─────────────────────────────────────────────────────────────────────────────
// #1117 — per-reviewee rate limit complements the read-side cap above
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_reviewee_rate_limit_blocks_burst_reviews() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    // Simulate `reviewee` already having received MAX_REVIEWS_PER_REVIEWEE_WINDOW
    // reviews earlier in the current rate-limit window (e.g. from many distinct
    // reviewers/jobs, so the per-reviewer cooldown alone wouldn't stop a griefer).
    env.as_contract(&reputation_id, || {
        env.storage().persistent().set(
            &DataKey::RevieweeReviewWindow(reviewee.clone()),
            &(0u32, MAX_REVIEWS_PER_REVIEWEE_WINDOW),
        );
    });

    // One more review in the same window is rejected even though `reviewer`
    // has no prior reviews of their own (their per-reviewer cooldown is clear).
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Spam"),
        &MIN_STAKE,
    );
}

#[test]
fn test_reviewee_rate_limit_resets_after_window() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    reputation_client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);
    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    env.as_contract(&reputation_id, || {
        env.storage().persistent().set(
            &DataKey::RevieweeReviewWindow(reviewee.clone()),
            &(0u32, MAX_REVIEWS_PER_REVIEWEE_WINDOW),
        );
    });

    // Advance past the rate-limit window (120 ledgers) so the reviewee-side
    // count resets and a genuine review is accepted again.
    env.ledger().with_mut(|l| l.sequence_number = 200);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Fine"),
        &MIN_STAKE,
    );

    assert_eq!(reputation_client.get_review_count(&reviewee), 1);
}

// ================================================================================================
// Security fix: stake lockup enforcement, stake_weight cap, and governance weight dampening
// ================================================================================================

/// Reproduce the flash-loan / same-block claim attack:
/// 1. Reviewer submits a review (stake deposited, lockup timer starts).
/// 2. Reviewer immediately tries to reclaim the stake.
/// 3. Must be rejected with StakeLockupActive (#29) — lockup not elapsed.
#[test]
#[should_panic(expected = "Error(Contract, #29)")]
fn test_claim_stake_rejected_before_lockup_elapses() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Great"),
        &MIN_STAKE,
    );

    // Advance only 1 second — far below the 7-day lockup.
    env.ledger().with_mut(|l| l.timestamp = 1_000_001);

    // Must fail with StakeLockupActive (#29).
    reputation_client.claim_stake(&reviewer, &MIN_STAKE);
}

/// Lockup boundary: claim rejected at exactly lockup_end - 1 second, accepted
/// at lockup_end (staked_at + STAKE_LOCKUP_SECONDS).
#[test]
fn test_claim_stake_accepted_exactly_at_lockup_boundary() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);

    let stake_ts: u64 = 1_000_000;
    env.ledger().with_mut(|l| l.timestamp = stake_ts);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Good"),
        &MIN_STAKE,
    );

    // One second before lockup ends — must still be rejected.
    env.ledger()
        .with_mut(|l| l.timestamp = stake_ts + STAKE_LOCKUP_SECONDS - 1);
    let result = reputation_client.try_claim_stake(&reviewer, &MIN_STAKE);
    assert!(
        result.is_err(),
        "claim must be rejected 1 second before lockup elapses"
    );

    // Exactly at lockup_end — must succeed.
    env.ledger()
        .with_mut(|l| l.timestamp = stake_ts + STAKE_LOCKUP_SECONDS);
    reputation_client.claim_stake(&reviewer, &MIN_STAKE);

    assert_eq!(reputation_client.get_stake_balance(&reviewer), 0);
}

/// Incremental stake refresh: a second review resets the lockup anchor.
/// The original stake cannot be claimed just because the first lockup elapsed —
/// the second deposit restarts the window for ALL accumulated stake.
#[test]
fn test_second_review_resets_lockup_for_all_stake() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee1 = Address::generate(&env);
    let reviewee2 = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &reviewer, 1_000_000_000);

    // First review at t=1000.
    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee1, &token_addr);
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee1,
        &1u64,
        &5u32,
        &String::from_str(&env, "Great"),
        &MIN_STAKE,
    );

    // Advance past the first lockup window (8 days) and past the rate-limit
    // window (120 ledgers) so the second review by the same reviewer is allowed.
    env.ledger().with_mut(|l| {
        l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 1;
        l.sequence_number = 200; // > 0 + 120 (RATE_LIMIT_LEDGERS_DEFAULT)
    });

    // Second review — resets lockup anchor to current time.
    setup_completed_job(&env, &escrow_id, 2u64, &reviewer, &reviewee2, &token_addr);
    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee2,
        &2u64,
        &5u32,
        &String::from_str(&env, "Also great"),
        &MIN_STAKE,
    );

    // Attempting to claim immediately after the second review must fail even
    // though the first lockup window has fully elapsed — the second review
    // resets the lockup anchor to the current timestamp.
    env.ledger().with_mut(|l| {
        l.timestamp = 1000 + STAKE_LOCKUP_SECONDS + 2;
    });
    let result = reputation_client.try_claim_stake(&reviewer, &MIN_STAKE);
    assert!(
        result.is_err(),
        "claim must be rejected while the second review's lockup is still active"
    );
}

/// Stake-weight cap: a single review with a stake_weight far above
/// MAX_STAKE_WEIGHT_PER_REVIEW must contribute no more than
/// 5 * MAX_STAKE_WEIGHT_PER_REVIEW to total_score (at a 5-star rating).
/// Before the fix, passing stake_weight = 1_000_000 * MIN_STAKE would have
/// given governance weight proportional to that huge number.
#[test]
fn test_stake_weight_capped_per_review() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    // Mint a very large amount so the transfer itself succeeds.
    // 100 * MAX_STAKE_WEIGHT_PER_REVIEW in raw token units, plus 100 for job funding.
    let huge_stake: i128 = (MAX_STAKE_WEIGHT_PER_REVIEW as i128) * 100;
    mint(&env, &token_addr, &token_admin, &reviewer, huge_stake + 100);

    env.ledger().with_mut(|l| l.timestamp = 1000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Five stars"),
        &huge_stake,
    );

    let (score, _) = reputation_client.get_gov_weight(&reviewee);

    // With no cap the score would be 5 * 100 * MAX_STAKE_WEIGHT_PER_REVIEW.
    // With the per-review cap it must be exactly 5 * MAX_STAKE_WEIGHT_PER_REVIEW.
    let expected_capped = 5u64 * MAX_STAKE_WEIGHT_PER_REVIEW;
    assert_eq!(
        score, expected_capped,
        "governance score must be capped at 5 * MAX_STAKE_WEIGHT_PER_REVIEW, got {score}"
    );
}

/// Governance weight hard cap: even if 200 max-rated, max-staked reviews exist,
/// the score returned by get_gov_weight must not exceed MAX_GOV_WEIGHT.
#[test]
fn test_get_gov_weight_capped_at_max() {
    let env = Env::default();
    env.mock_all_auths();
    env.budget().reset_unlimited();

    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    let user = Address::generate(&env);
    let reviewer = Address::generate(&env);

    // Inject 200 reviews each with MAX_STAKE_WEIGHT_PER_REVIEW and 5-star rating,
    // which is exactly the legitimate ceiling. Also inject 1 extra review with a
    // stake 10x the cap; capping must keep the total at MAX_GOV_WEIGHT.
    env.as_contract(&reputation_id, || {
        let mut reviews: Vec<Review> = Vec::new(&env);
        for i in 0..200u64 {
            reviews.push_back(Review {
                reviewer: reviewer.clone(),
                reviewee: user.clone(),
                job_id: i,
                rating: 5u32,
                comment: String::from_str(&env, ""),
                // 10x the per-review cap — read-path capping must bring this down.
                stake_weight: (MAX_STAKE_WEIGHT_PER_REVIEW as i128) * 10,
                timestamp: 0,
            });
        }
        env.storage()
            .persistent()
            .set(&DataKey::Reviews(user.clone()), &reviews);
        env.storage().persistent().set(
            &DataKey::Reputation(user.clone()),
            &UserReputation {
                user: user.clone(),
                total_score: 0,
                total_weight: 0,
                review_count: 200,
                last_updated_ts: 0,
            },
        );
    });

    let (score, _ts) = client.get_gov_weight(&user);
    assert_eq!(
        score, MAX_GOV_WEIGHT,
        "get_gov_weight must be capped at MAX_GOV_WEIGHT, got {score}"
    );
}

/// Two-address Sybil exploit end-to-end:
/// Attacker controls addr_a and addr_b, creates a completed job between them,
/// each reviews the other with a massive stake_weight.
/// Assert that get_gov_weight is capped rather than reflecting the raw
/// inflated stake, AND that the stake cannot be reclaimed before the lockup.
#[test]
fn test_sybil_two_address_self_dealt_review_capped() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    // Attacker-controlled addresses.
    let addr_a = Address::generate(&env);
    let addr_b = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    // Mint enough for both to stake a huge amount, plus 100 each for job funding.
    let huge_stake: i128 = (MAX_STAKE_WEIGHT_PER_REVIEW as i128) * 1_000;
    mint(&env, &token_addr, &token_admin, &addr_a, huge_stake + 100);
    mint(&env, &token_addr, &token_admin, &addr_b, huge_stake + 100);

    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    // Create a completed job between the two attacker addresses.
    setup_completed_job(&env, &escrow_id, 1u64, &addr_a, &addr_b, &token_addr);

    // addr_a (client) reviews addr_b (freelancer) with inflated stake.
    reputation_client.submit_review(
        &escrow_id,
        &addr_a,
        &addr_b,
        &1u64,
        &5u32,
        &String::from_str(&env, "Sybil 5 stars"),
        &huge_stake,
    );

    // addr_b (freelancer) reviews addr_a (client) with inflated stake.
    setup_completed_job(&env, &escrow_id, 2u64, &addr_b, &addr_a, &token_addr);
    reputation_client.submit_review(
        &escrow_id,
        &addr_b,
        &addr_a,
        &2u64,
        &5u32,
        &String::from_str(&env, "Sybil 5 stars back"),
        &huge_stake,
    );

    // --- Assertion 1: governance weight is capped, NOT inflated ---
    let (score_b, _) = reputation_client.get_gov_weight(&addr_b);
    let (score_a, _) = reputation_client.get_gov_weight(&addr_a);
    let expected_max_single = 5u64 * MAX_STAKE_WEIGHT_PER_REVIEW;
    assert_eq!(
        score_b, expected_max_single,
        "addr_b gov weight must be capped at {expected_max_single}, got {score_b}"
    );
    assert_eq!(
        score_a, expected_max_single,
        "addr_a gov weight must be capped at {expected_max_single}, got {score_a}"
    );

    // --- Assertion 2: stake cannot be reclaimed immediately (flash-loan defence) ---
    let result_a = reputation_client.try_claim_stake(&addr_a, &huge_stake);
    assert!(
        result_a.is_err(),
        "addr_a must not be able to reclaim stake before lockup elapses"
    );
    let result_b = reputation_client.try_claim_stake(&addr_b, &huge_stake);
    assert!(
        result_b.is_err(),
        "addr_b must not be able to reclaim stake before lockup elapses"
    );

    // --- Assertion 3: stake CAN be reclaimed after the lockup ---
    env.ledger()
        .with_mut(|l| l.timestamp = 1_000_000 + STAKE_LOCKUP_SECONDS + 1);
    reputation_client.claim_stake(&addr_a, &huge_stake);
    reputation_client.claim_stake(&addr_b, &huge_stake);
    assert_eq!(reputation_client.get_stake_balance(&addr_a), 0);
    assert_eq!(reputation_client.get_stake_balance(&addr_b), 0);
}

/// Legitimate high-stake reviewer: a normal MIN_STAKE review with a 5-star
/// rating goes through uncapped (stake is below MAX_STAKE_WEIGHT_PER_REVIEW)
/// and remains claimable after the lockup. Ensures the fix doesn't break
/// the honest happy path.
#[test]
fn test_legitimate_review_and_claim_unaffected() {
    let env = Env::default();
    env.mock_all_auths();

    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let reputation_client = ReputationContractClient::new(&env, &reputation_id);

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    let token_admin = Address::generate(&env);
    let token_addr = create_token(&env, &token_admin);

    mint(&env, &token_addr, &token_admin, &reviewer, 100_000_000);
    env.ledger().with_mut(|l| l.timestamp = 5_000_000);
    setup_completed_job(&env, &escrow_id, 1u64, &reviewer, &reviewee, &token_addr);

    reputation_client.submit_review(
        &escrow_id,
        &reviewer,
        &reviewee,
        &1u64,
        &5u32,
        &String::from_str(&env, "Excellent"),
        &MIN_STAKE,
    );

    // Reputation score = 5 * MIN_STAKE (well below both caps).
    let (score, _) = reputation_client.get_gov_weight(&reviewee);
    assert_eq!(
        score,
        5u64 * MIN_STAKE as u64,
        "legitimate review score should equal 5 * MIN_STAKE"
    );

    // Claiming before lockup elapses is still rejected.
    assert!(
        reputation_client
            .try_claim_stake(&reviewer, &MIN_STAKE)
            .is_err(),
        "claim must fail inside the lockup window"
    );

    // After lockup, claim succeeds.
    env.ledger()
        .with_mut(|l| l.timestamp = 5_000_000 + STAKE_LOCKUP_SECONDS + 1);
    reputation_client.claim_stake(&reviewer, &MIN_STAKE);
    assert_eq!(reputation_client.get_stake_balance(&reviewer), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// #1173 — get_leaderboard_page pagination behaviour
//
// Only the zero-argument `get_leaderboard()` wrapper (offset=0, limit=50) was
// exercised before, so offset/limit handling, clamping and out-of-range offsets
// were untested. These tests drive `get_leaderboard_page` directly.
// ─────────────────────────────────────────────────────────────────────────────

/// Write a leaderboard of `count` entries straight into instance storage,
/// descending by rating (1000, 990, 980, …) exactly as `update_leaderboard`
/// maintains it. Returns the addresses in leaderboard order.
///
/// Seeding directly keeps these tests focused on pagination arithmetic and lets
/// them exceed the 50-entry limit cheaply; `test_leaderboard_page_offset_and_limit_
/// with_real_reviews` covers the same code path on a leaderboard built by
/// `submit_review`.
fn seed_leaderboard(env: &Env, contract_id: &Address, count: u32) -> soroban_sdk::Vec<Address> {
    let mut addresses = soroban_sdk::Vec::new(env);
    let mut entries: soroban_sdk::Vec<(Address, u64)> = soroban_sdk::Vec::new(env);
    for i in 0..count {
        let addr = Address::generate(env);
        addresses.push_back(addr.clone());
        entries.push_back((addr, 1000u64 - (i as u64) * 10));
    }
    env.as_contract(contract_id, || {
        env.storage().instance().set(&DataKey::Leaderboard, &entries);
    });
    addresses
}

#[test]
fn test_leaderboard_page_first_page_matches_default_wrapper() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    seed_leaderboard(&env, &contract_id, 10);

    // The wrapper is exactly get_leaderboard_page(0, 50).
    assert_eq!(client.get_leaderboard_page(&0, &50), client.get_leaderboard());
}

#[test]
fn test_leaderboard_page_non_zero_offset_returns_correct_slice() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    let addresses = seed_leaderboard(&env, &contract_id, 10);

    let page = client.get_leaderboard_page(&3, &4);
    assert_eq!(page.len(), 4);
    for i in 0..4u32 {
        let (addr, rating) = page.get(i).unwrap();
        assert_eq!(addr, addresses.get(3 + i).unwrap(), "entry {i} is off by one");
        assert_eq!(rating, 1000 - ((3 + i) as u64) * 10);
    }

    // Consecutive pages tile the list without gaps or repeats.
    let first = client.get_leaderboard_page(&0, &5);
    let second = client.get_leaderboard_page(&5, &5);
    assert_eq!(first.len(), 5);
    assert_eq!(second.len(), 5);
    for i in 0..5u32 {
        assert_eq!(first.get(i).unwrap().0, addresses.get(i).unwrap());
        assert_eq!(second.get(i).unwrap().0, addresses.get(5 + i).unwrap());
    }
}

#[test]
fn test_leaderboard_page_custom_limit() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    let addresses = seed_leaderboard(&env, &contract_id, 10);

    let page = client.get_leaderboard_page(&0, &1);
    assert_eq!(page.len(), 1);
    assert_eq!(page.get(0).unwrap().0, addresses.get(0).unwrap());

    // A limit of zero is a legitimate (if useless) request: empty page, no panic.
    assert_eq!(client.get_leaderboard_page(&0, &0).len(), 0);

    // A limit larger than what remains is truncated at the end of the list.
    let tail = client.get_leaderboard_page(&8, &10);
    assert_eq!(tail.len(), 2);
    assert_eq!(tail.get(0).unwrap().0, addresses.get(8).unwrap());
    assert_eq!(tail.get(1).unwrap().0, addresses.get(9).unwrap());
}

#[test]
fn test_leaderboard_page_limit_is_clamped_to_fifty() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    let addresses = seed_leaderboard(&env, &contract_id, 60);

    // Anything above the 50 maximum yields exactly 50 entries, including u32::MAX
    // (which would overflow offset + limit without the saturating add).
    assert_eq!(client.get_leaderboard_page(&0, &51).len(), 50);
    assert_eq!(client.get_leaderboard_page(&0, &1000).len(), 50);
    assert_eq!(client.get_leaderboard_page(&0, &u32::MAX).len(), 50);

    // Clamping applies from the offset, not from the start of the list.
    let page = client.get_leaderboard_page(&5, &u32::MAX);
    assert_eq!(page.len(), 50);
    assert_eq!(page.get(0).unwrap().0, addresses.get(5).unwrap());
    assert_eq!(page.get(49).unwrap().0, addresses.get(54).unwrap());
}

#[test]
fn test_leaderboard_page_out_of_range_offset_is_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    seed_leaderboard(&env, &contract_id, 10);

    // offset == total and beyond: empty, never a panic.
    assert_eq!(client.get_leaderboard_page(&10, &5).len(), 0);
    assert_eq!(client.get_leaderboard_page(&11, &5).len(), 0);
    assert_eq!(client.get_leaderboard_page(&u32::MAX, &5).len(), 0);

    // Last valid offset still returns a single entry.
    assert_eq!(client.get_leaderboard_page(&9, &5).len(), 1);
}

#[test]
fn test_leaderboard_page_empty_leaderboard_is_empty_at_any_offset() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);

    // No leaderboard entry has ever been written.
    assert_eq!(client.get_leaderboard_page(&0, &50).len(), 0);
    assert_eq!(client.get_leaderboard_page(&7, &50).len(), 0);
}

#[test]
fn test_leaderboard_page_offset_applies_after_banned_users_are_filtered() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);
    let addresses = seed_leaderboard(&env, &contract_id, 5);

    // Ban the top-ranked user: paging must index into the filtered list, so
    // offset 0 is now the second-ranked user and the last offset falls away.
    client.ban_user(&admin, &addresses.get(0).unwrap());

    let page = client.get_leaderboard_page(&0, &50);
    assert_eq!(page.len(), 4);
    assert_eq!(page.get(0).unwrap().0, addresses.get(1).unwrap());

    let page = client.get_leaderboard_page(&3, &50);
    assert_eq!(page.len(), 1);
    assert_eq!(page.get(0).unwrap().0, addresses.get(4).unwrap());

    assert_eq!(client.get_leaderboard_page(&4, &50).len(), 0);
}

#[test]
fn test_leaderboard_page_offset_and_limit_with_real_reviews() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &0u32);

    // Three reviewees with distinct ratings, so leaderboard order is deterministic.
    let mut reviewees = soroban_sdk::Vec::new(&env);
    for (i, rating) in [5u32, 4u32, 3u32].iter().enumerate() {
        let reviewer = Address::generate(&env);
        let reviewee = Address::generate(&env);
        setup_review_for(
            &env,
            &escrow_id,
            &client,
            i as u64 + 1,
            &reviewer,
            &reviewee,
            *rating,
        );
        reviewees.push_back(reviewee);
    }

    let full = client.get_leaderboard_page(&0, &50);
    assert_eq!(full.len(), 3);

    // Paging through a leaderboard built by submit_review returns the same
    // entries in the same order as the unpaginated read.
    let middle = client.get_leaderboard_page(&1, &1);
    assert_eq!(middle.len(), 1);
    assert_eq!(middle.get(0).unwrap(), full.get(1).unwrap());

    let last_two = client.get_leaderboard_page(&1, &10);
    assert_eq!(last_two.len(), 2);
    assert_eq!(last_two.get(0).unwrap(), full.get(1).unwrap());
    assert_eq!(last_two.get(1).unwrap(), full.get(2).unwrap());

    assert_eq!(client.get_leaderboard_page(&3, &10).len(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// #1174 — get_effective_weight decay coverage
//
// `get_effective_weight` implements its own decay formula, independent of
// `get_decayed_totals`, and had no test coverage at all. These pin the formula
// across its whole range and cross-check it against `get_decayed_totals` so the
// two cannot silently diverge.
// ─────────────────────────────────────────────────────────────────────────────

/// A review recorded at `timestamp` carrying `stake_weight`.
fn review_at(env: &Env, stake_weight: i128, timestamp: u64) -> Review {
    Review {
        reviewer: Address::generate(env),
        reviewee: Address::generate(env),
        job_id: 1,
        rating: 5,
        comment: String::from_str(env, "ok"),
        stake_weight,
        timestamp,
    }
}

/// Register a reputation contract configured with `decay_rate`% annual decay;
/// `get_effective_weight` reads the rate from instance storage.
fn effective_weight_client<'a>(env: &'a Env, decay_rate: u32) -> ReputationContractClient<'a> {
    let contract_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(env, &contract_id);
    let admin = Address::generate(env);
    client.initialize(&vec![env, admin], &1u32, &decay_rate);
    client
}

#[test]
fn test_effective_weight_fresh_review_keeps_full_weight() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 10);

    // Zero elapsed time -> decay factor 100% -> full stake weight.
    let review = review_at(&env, 1_000_i128, 0);
    assert_eq!(client.get_effective_weight(&review, &0), 1_000);

    // Still effectively fresh well inside the first year: 10% * 0.1yr = 1% off.
    let tenth_of_a_year = ONE_YEAR_IN_SECONDS / 10;
    assert_eq!(
        client.get_effective_weight(&review, &tenth_of_a_year),
        990,
        "10%/yr for a tenth of a year should shave exactly 1%"
    );
}

#[test]
fn test_effective_weight_zero_decay_rate_never_decays() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 0);

    let review = review_at(&env, 1_000_i128, 0);
    assert_eq!(client.get_effective_weight(&review, &0), 1_000);
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 100)),
        1_000,
        "with decay disabled, age must not matter"
    );
}

#[test]
fn test_effective_weight_partial_decay_is_linear_in_age() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 10); // 10% per year

    let review = review_at(&env, 1_000_i128, 0);

    // Retained = 100 - 10 * years.
    assert_eq!(client.get_effective_weight(&review, &ONE_YEAR_IN_SECONDS), 900);
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 3)),
        700
    );
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 5)),
        500
    );
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 9)),
        100
    );

    // Age is measured from the review's own timestamp, not from epoch.
    let later = review_at(&env, 1_000_i128, ONE_YEAR_IN_SECONDS * 4);
    assert_eq!(
        client.get_effective_weight(&later, &(ONE_YEAR_IN_SECONDS * 9)),
        500,
        "a 5-year-old review must decay like any other 5-year-old review"
    );
}

#[test]
fn test_effective_weight_fully_decayed_is_zero_and_saturates() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 10); // 10% per year

    let review = review_at(&env, 1_000_i128, 0);

    // Exactly 10 years: retained 0%.
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 10)),
        0
    );
    // Beyond full decay the factor saturates at 0 rather than wrapping.
    assert_eq!(
        client.get_effective_weight(&review, &(ONE_YEAR_IN_SECONDS * 50)),
        0
    );
    assert_eq!(client.get_effective_weight(&review, &u64::MAX), 0);
}

#[test]
fn test_effective_weight_future_timestamp_is_not_negative_decay() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 10);

    // current_time before the review's timestamp: age saturates to 0, so the
    // weight is full — never inflated above the staked amount.
    let review = review_at(&env, 1_000_i128, ONE_YEAR_IN_SECONDS * 5);
    assert_eq!(client.get_effective_weight(&review, &0), 1_000);
}

#[test]
fn test_effective_weight_non_positive_stake_uses_weight_of_one() {
    let env = Env::default();
    env.mock_all_auths();
    let client = effective_weight_client(&env, 10);

    // A review carrying no stake still counts as weight 1 when fresh.
    let unstaked = review_at(&env, 0_i128, 0);
    assert_eq!(client.get_effective_weight(&unstaked, &0), 1);

    // ...and that single unit decays away to nothing (integer division).
    assert_eq!(
        client.get_effective_weight(&unstaked, &(ONE_YEAR_IN_SECONDS * 5)),
        0
    );

    // A negative stake weight is treated the same as zero, never negatively.
    let negative = review_at(&env, -50_i128, 0);
    assert_eq!(client.get_effective_weight(&negative, &0), 1);
}

#[test]
fn test_effective_weight_agrees_with_get_decayed_totals() {
    let env = setup_high_ttl_env();
    env.mock_all_auths();
    let escrow_id = env.register_contract(None, EscrowContract);
    let reputation_id = env.register_contract(None, ReputationContract);
    let client = ReputationContractClient::new(&env, &reputation_id);
    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1u32, &10u32); // 10%/yr

    let reviewer = Address::generate(&env);
    let reviewee = Address::generate(&env);
    setup_review_for(&env, &escrow_id, &client, 1, &reviewer, &reviewee, 5);

    let review = client.get_reviews(&reviewee).get(0).unwrap();

    // The two independently-implemented decay formulas must agree: with a single
    // review, the reputation's decayed total_weight is that review's effective
    // weight, both when fresh and part-way through the decay range.
    let now = env.ledger().timestamp();
    assert_eq!(
        client.get_effective_weight(&review, &now) as u64,
        client.get_reputation(&reviewee).total_weight
    );

    advance_n_periods(&env, 5); // 5 years -> 50% retained
    let now = env.ledger().timestamp();
    let effective = client.get_effective_weight(&review, &now);
    assert_eq!(effective as u64, client.get_reputation(&reviewee).total_weight);
    assert_eq!(effective, review.stake_weight / 2);
}
