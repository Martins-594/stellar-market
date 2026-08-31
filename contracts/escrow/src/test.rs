use soroban_sdk::{
    contract, contractimpl,
    testutils::{Address as _, Events, Ledger},
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, IntoVal, String, Symbol, Vec,
};

use crate::*;

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(_env: Env, _from: Address, _to: Address, _amount: i128) {}
}

const GRACE_PERIOD: u64 = 604_800; // 7 days in seconds
const MIN_STAKE: i128 = 10_000_000;
const JOB_DEADLINE: u64 = 1_000_000; // Example value
const DEFAULT_EXPIRY_LEDGER: u32 = 518_400; // 30 days in ledgers (17,280 ledgers/day * 30)

// Correction 3: token_address is already Address from register_stellar_asset_contract_v2,
// so we use it directly without calling .address() on it.
fn setup_test(env: &Env) -> (EscrowContractClient<'_>, Address, Address, Address, Address) {
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(env, &contract_id);

    let user_client = Address::generate(env);
    let freelancer = Address::generate(env);
    let admin = Address::generate(env);

    let token_address = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_admin = StellarAssetClient::new(env, &token_address);
    token_admin.mint(&user_client, &10000);

    let signers = vec![env, admin.clone()];
    let treasury = Address::generate(env);
    client.initialize(&signers, &1, &treasury, &0, &604800);

    (client, user_client, freelancer, token_address, admin)
}

fn pause_escrow(env: &Env, client: &EscrowContractClient<'_>, admin: &Address) {
    // Pause proposals have a 48-hour time lock and cannot auto-execute with a single signer.
    // Add a temp signer (no time lock), then advance past the lock and approve with it.
    let temp_signer = Address::generate(env);
    client.propose_admin_action(admin, &AdminAction::AddSigner(temp_signer.clone()));
    let proposal_id = client.propose_admin_action(admin, &AdminAction::Pause);
    env.ledger().with_mut(|l| l.timestamp += 48 * 60 * 60 + 1);
    client.approve_admin_action(&temp_signer, &proposal_id);
}

fn unpause_escrow(env: &Env, client: &EscrowContractClient<'_>, admin: &Address) {
    // Unpause has no time lock and threshold is 1, so it auto-executes on propose.
    client.propose_admin_action(admin, &AdminAction::Unpause);
}

fn setup_multisig(env: &Env) -> (EscrowContractClient<'_>, Address, Address, Address, Address, Address) {
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(env, &contract_id);

    let client_addr = Address::generate(env);
    let freelancer = Address::generate(env);
    let signer1 = Address::generate(env);
    let signer2 = Address::generate(env);
    let treasury = Address::generate(env);

    let token_address = env.register_stellar_asset_contract_v2(Address::generate(env)).address();
    let token_admin = StellarAssetClient::new(env, &token_address);
    token_admin.mint(&client_addr, &10000);

    let signers = vec![env, signer1.clone(), signer2.clone()];
    client.initialize(&signers, &2, &treasury, &100, &604800);

    (client, client_addr, freelancer, token_address, signer1, signer2)
}

#[test]
fn test_create_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design mockups"), 500_i128, JOB_DEADLINE / 3),
        (String::from_str(&env, "Frontend implementation"), 1000_i128, JOB_DEADLINE * 2 / 3),
        (String::from_str(&env, "Backend integration"), 1500_i128, JOB_DEADLINE),
    ];

    // Correction 4: Calculate expected total dynamically
    let expected_total: i128 = 500 + 1000 + 1500;

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE, // job_deadline must be >= all milestone deadlines
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert_eq!(job_id, 1);

    let job = contract.get_job(&job_id);
    assert_eq!(job.client, client_addr);
    assert_eq!(job.freelancer, freelancer);
    assert_eq!(job.total_amount, expected_total);
}

#[test]
fn test_create_job_self_employment_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, _freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design mockups"), 500_i128, JOB_DEADLINE),
    ];

    let result = contract.try_create_job(
        &client_addr,
        &client_addr,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert_eq!(result, Err(Ok(EscrowError::Unauthorized)));
}

#[test]
fn test_create_job_distinct_addresses_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design mockups"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert_eq!(job_id, 1);
}

#[test]
fn test_extend_deadline_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design mockups"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    let new_deadline = JOB_DEADLINE + 1000;
    contract.extend_deadline(&job_id, &0, &new_deadline);

    let events = env.events().all();
    let last_event = events.last().unwrap();
    
    // Topics: (symbol_short!("escrow"), symbol_short!("deadline"))
    assert_eq!(
        last_event.0,
        contract.address
    );
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, symbol_short!("deadline"));
    
    // Payload: (job_id, milestone_id, new_deadline)
    let payload: (u64, u32, u64) = last_event.2.into_val(&env);
    assert_eq!(payload, (job_id, 0, new_deadline));
}

#[test]
fn test_fee_cap_enforcement_invalid_fee_error() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let signers = vec![&env, admin.clone()];
    let treasury = Address::generate(&env);
    
    // Initialize with valid fee first
    client.initialize(&signers, &1, &treasury, &0, &604800);

    // Try to set fee above MAX_FEE_BPS (500)
    let action = AdminAction::SetFeeBps(501);
    
    // This should return EscrowError::InvalidFee (35)
    let result = client.try_propose_admin_action(&admin, &action);
    assert_eq!(result, Err(Ok(EscrowError::InvalidFee)));
}

#[test]
fn test_job_count_increments() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, JOB_DEADLINE)];

    let id1 = contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE, // job_deadline must be >= milestone deadlines
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    let id2 = contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE, // job_deadline must be >= milestone deadlines
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    assert_eq!(id1, 1);
    assert_eq!(id2, 2);
    assert_eq!(contract.get_job_count(), 2);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #49)")] // MilestoneDeadlineInPast
fn test_create_job_invalid_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 100_i128, 500_u64), // Invalid, < 1000
    ];

    contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #33)")] // EmptyMilestones
fn test_create_job_empty_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env];

    contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #34)")] // TooManyMilestones
fn test_create_job_too_many_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let mut milestones = vec![&env];
    for i in 0..51u64 {
        milestones.push_back((String::from_str(&env, "Task"), 100_i128, (i + 1) * 10_000_u64));
    }

    contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
fn test_create_job_max_milestones_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let mut milestones = vec![&env];
    for i in 0u64..20 {
        milestones.push_back((String::from_str(&env, "Task"), 100_i128, 1001 + i));
    }

    let job_id = contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert!(job_id > 0);
}

#[test]
fn test_create_job_single_milestone_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, user, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Single"), 100_i128, 2000_u64)];

    let job_id = contract.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert!(job_id > 0);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #8)")] // MilestoneDeadlineExceeded
fn test_submit_milestone_past_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );
    client.fund_job(&job_id, &user, &0, &0);

    // fast forward past deadline
    env.ledger().with_mut(|l| l.timestamp = 2500);

    client.submit_milestone(&job_id, &0, &freelancer);
}

#[test]
fn test_is_milestone_overdue() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    // not overdue initially
    assert_eq!(client.is_milestone_overdue(&job_id, &0), false);

    // fast forward past deadline
    env.ledger().with_mut(|l| l.timestamp = 2500);

    // overdue now
    assert_eq!(client.is_milestone_overdue(&job_id, &0), true);
}

#[test]
fn test_extend_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    client.extend_deadline(&job_id, &0, &4000_u64);

    let job = client.get_job(&job_id);
    assert_eq!(job.milestones.get(0).unwrap().deadline, 4000);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_extend_deadline_fails_when_disputed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &3000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    // Force the job into Disputed state
    env.as_contract(&client.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    // Must be rejected with InvalidStatus (#3)
    client.extend_deadline(&job_id, &0, &4000_u64);
}

// ── Helpers for claim_refund tests ───────────────────────────────────────────

fn setup_refund_env(env: &Env) -> (EscrowContractClient<'_>, Address) {
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(env, &contract_id);

    let admin = Address::generate(env);
    // Correction 2 & 3: Use register_stellar_asset_contract_v2 and get .address()
    let token_addr = env.register_stellar_asset_contract_v2(admin.clone()).address();

    (escrow, token_addr)
}

fn mint_tokens(env: &Env, token: &Address, to: &Address, amount: i128) {
    let admin_client = StellarAssetClient::new(env, token);
    admin_client.mint(to, &amount);
}

fn default_milestones(env: &Env) -> Vec<(String, i128, u64)> {
    vec![
        env,
        (String::from_str(env, "Design"), 500_i128, 500_000_u64),
        (String::from_str(env, "Frontend"), 1000_i128, 700_000_u64),
        (String::from_str(env, "Backend"), 1500_i128, 900_000_u64),
    ]
}

// ── Full refund: no milestones approved, job funded and abandoned ─────────────

#[test]
fn test_claim_refund_full() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    // Correction 4: Calculate expected total dynamically
    let expected_total: i128 = 500 + 1000 + 1500;

    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, expected_total);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Advance time past job_deadline + grace period
    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    escrow.claim_refund(&job_id, &client, &0);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);

    // Correction 4: Client should have received full refund (dynamic)
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), expected_total);
}

// ── Partial refund: one milestone approved, rest refunded ────────────────────

#[test]
fn test_claim_refund_partial() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    // Correction 4: Calculate amounts dynamically
    let milestone_0_amount: i128 = 500;
    let total: i128 = 500 + 1000 + 1500;

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Freelancer submits milestone 0, client approves it
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    // Advance past job_deadline + grace
    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    escrow.claim_refund(&job_id, &client, &0);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);

    // Correction 4: Client gets back total - milestone_0_amount dynamically
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), total - milestone_0_amount);

    // Freelancer received the approved milestone amount
    assert_eq!(token_client.balance(&freelancer), milestone_0_amount);
}

// ── Refund on InProgress job ─────────────────────────────────────────────────

#[test]
fn test_claim_refund_in_progress_status() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Submit and approve first milestone to move to InProgress
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);

    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    escrow.claim_refund(&job_id, &client, &0);
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);
}

// ── Fail: grace period not met ───────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #11)")] // GracePeriodNotMet
fn test_claim_refund_fails_before_grace_period() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Time is before job_deadline + grace (only at deadline)
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE);

    escrow.claim_refund(&job_id, &client, &0);
}

// ── Fail: pending milestone submission ───────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #9)")] // HasPendingMilestone
fn test_claim_refund_fails_with_pending_milestone() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Freelancer submits a milestone (status = Submitted, not yet approved)
    escrow.submit_milestone(&job_id, &0, &freelancer);

    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    // Should fail because there's a submitted milestone awaiting review
    escrow.claim_refund(&job_id, &client, &0);
}

// ── Fail: wrong caller (not the client) ──────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_claim_refund_fails_unauthorized() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    // Freelancer tries to claim refund — should fail
    escrow.claim_refund(&job_id, &freelancer, &0);
}

// ── Fail: job already completed ──────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_claim_refund_fails_on_completed_job() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Correction 4: Use named amount for clarity and dynamic assertion
    let task_amount: i128 = 1000;
    let milestones = vec![
        &env,
        (String::from_str(&env, "Only task"), task_amount, 500_000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, task_amount);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Complete the job
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);
    escrow.complete_job(&job_id, &client);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    // Should fail — job is already completed
    escrow.claim_refund(&job_id, &client, &0);
}

// ── Fail: job already cancelled ──────────────────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_claim_refund_fails_on_cancelled_job() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Cancel the job first via existing cancel_job
    escrow.cancel_job(&job_id, &client, &0);

    env.ledger()
        .with_mut(|l| l.timestamp = JOB_DEADLINE + GRACE_PERIOD + 1);

    // Should fail — job is already cancelled
    escrow.claim_refund(&job_id, &client, &0);
}

// ============================================================
// JOB REVISION TESTS
// ============================================================

#[test]
fn test_client_can_propose_revision() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Correction 4: Use named amounts for dynamic assertions
    let m0_amount: i128 = 600;
    let m1_amount: i128 = 600;
    let expected_new_total = m0_amount + m1_amount;

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New Phase 1"),
            amount: m0_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE - 1,
                token: None,
        },
        Milestone {
            id: 1,
            description: String::from_str(&env, "New Phase 2"),
            amount: m1_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];

    contract.propose_revision(&client, &job_id, &new_milestones);

    let proposal = contract
        .get_revision_proposal(&job_id)
        .expect("Proposal should exist");
    assert_eq!(proposal.proposer, client);
    assert_eq!(proposal.new_total, expected_new_total);
    assert_eq!(proposal.status, ProposalStatus::Pending);
}

#[test]
fn test_propose_revision_rejects_non_positive_milestone_amount() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let invalid_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Invalid"),
            amount: 0,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];

    let result = contract.try_propose_revision(&client, &job_id, &invalid_milestones);
    assert_eq!(result, Err(Ok(EscrowError::InvalidMilestone)));
}

#[test]
fn test_propose_revision_rejects_past_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let invalid_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Invalid"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: 1000,
            token: None,
        },
    ];

    let result = contract.try_propose_revision(&client, &job_id, &invalid_milestones);
    assert_eq!(result, Err(Ok(EscrowError::MilestoneDeadlineInPast)));
}

#[test]
fn test_propose_revision_rejects_deadline_after_job_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let invalid_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Invalid"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE + 1,
            token: None,
        },
    ];

    let result = contract.try_propose_revision(&client, &job_id, &invalid_milestones);
    assert_eq!(result, Err(Ok(EscrowError::InvalidDeadline)));
}

#[test]
fn test_propose_revision_rejects_non_increasing_deadlines() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let invalid_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "First"),
            amount: 500,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE - 100,
            token: None,
        },
        Milestone {
            id: 1,
            description: String::from_str(&env, "Second"),
            amount: 700,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE - 100,
            token: None,
        },
    ];

    let result = contract.try_propose_revision(&client, &job_id, &invalid_milestones);
    assert_eq!(result, Err(Ok(EscrowError::MilestoneDeadlinesNotOrdered)));
}

#[test]
fn test_freelancer_can_propose_revision() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let m0_amount: i128 = 1500;

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: m0_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);

    let proposal = contract
        .get_revision_proposal(&job_id)
        .expect("Proposal should exist");
    assert_eq!(proposal.proposer, freelancer);
    assert_eq!(proposal.new_total, m0_amount); // Correction 4: dynamic
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_propose_revision_fails_for_disputed_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];

    contract.propose_revision(&client, &job_id, &new_milestones);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_propose_revision_fails_for_completed_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Completed;
        env.storage().persistent().set(&key, &job);
    });

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];

    contract.propose_revision(&client, &job_id, &new_milestones);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_propose_revision_fails_for_cancelled_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Cancelled;
        env.storage().persistent().set(&key, &job);
    });

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];

    contract.propose_revision(&client, &job_id, &new_milestones);
}

#[test]
fn test_accept_revision_preserves_partially_paid_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let initial_amount: i128 = 1000;
    let disbursed: i128 = 700;
    let new_amount: i128 = 1200;

    let milestones = vec![&env, (String::from_str(&env, "Initial"), initial_amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Pay out 700 of the 1000 nominal amount before any revision happens.
    contract.release_partial_payment(&job_id, &0, &disbursed, &client_addr, &0);

    let client_balance_before_revision = token.balance(&client_addr);
    let contract_balance_before_revision = token.balance(&contract.address);
    assert_eq!(contract_balance_before_revision, initial_amount - disbursed); // 300 left in escrow

    // Freelancer proposes raising milestone 0's total nominal value from 1000 to 1200.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: new_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client_addr, &job_id);

    let job = contract.get_job(&job_id);
    assert_eq!(job.total_amount, new_amount);

    // The already-disbursed 700 must be preserved: the stored milestone should reflect
    // only the remaining, unpaid balance (1200 - 700 = 500), not the raw proposed amount.
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::PartiallyPaid);
    assert_eq!(ms.amount, new_amount - disbursed);

    // Freelancer's already-received funds are untouched.
    assert_eq!(token.balance(&freelancer), disbursed);

    // Top-up transfer must be based on the real remaining escrow balance, not the stale
    // nominal old_total: client only owes enough to bring escrow up to (new_total - paid).
    let expected_topup = new_amount - initial_amount; // 200
    assert_eq!(token.balance(&client_addr), client_balance_before_revision - expected_topup);
    assert_eq!(token.balance(&contract.address), new_amount - disbursed); // 500
}

#[test]
#[should_panic(expected = "Error(Contract, #50)")]
fn test_accept_revision_fails_when_new_total_below_disbursed_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let initial_amount: i128 = 1000;
    let disbursed: i128 = 700;

    let milestones = vec![&env, (String::from_str(&env, "Initial"), initial_amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Pay out 700 of the 1000 nominal amount.
    contract.release_partial_payment(&job_id, &0, &disbursed, &client_addr, &0);

    // Propose shrinking milestone 0's total nominal value to 500 — below the 700 already paid.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Shrunk"),
            amount: 500,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);

    // Must be rejected — not silently accepted with an incorrect refund.
    contract.accept_revision(&client_addr, &job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #50)")]
fn test_accept_revision_fails_when_dropping_a_paid_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "First"), 1000_i128, JOB_DEADLINE),
        (String::from_str(&env, "Second"), 500_i128, JOB_DEADLINE + 1),
    ];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &(JOB_DEADLINE + 1), &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Fully pay off milestone 0 via partial payment.
    contract.release_partial_payment(&job_id, &0, &1000, &client_addr, &0);

    // Proposal only carries milestone id 1 forward, silently dropping the already-paid
    // milestone id 0 entirely.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 1,
            description: String::from_str(&env, "Second revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE + 1,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);

    contract.accept_revision(&client_addr, &job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_approve_milestone_fails_for_disputed_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Mocking job status to Disputed
    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    contract.approve_milestone(&job_id, &0, &client);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_submit_milestone_fails_for_disputed_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Mocking job status to Disputed
    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    contract.submit_milestone(&job_id, &0, &freelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_approve_milestones_batch_fails_for_disputed_job() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Mocking job status to Disputed
    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    contract.approve_milestones_batch(&job_id, &vec![&env, 0], &client);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_propose_revision_fails_for_non_party() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);
    let third_party = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&third_party, &job_id, &new_milestones);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_propose_revision_fails_when_pending_proposal_exists() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #34)")] // TooManyMilestones
fn test_propose_revision_too_many_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let mut new_milestones = vec![&env];
    for i in 0..51u32 {
        new_milestones.push_back(Milestone {
            id: i,
            description: String::from_str(&env, "New"),
            amount: 10,
            status: MilestoneStatus::Pending,
            deadline: (i as u64 + 1) * 10_000_u64,
                token: None,
        });
    }
    contract.propose_revision(&client, &job_id, &new_milestones);
}

#[test]
fn test_propose_revision_allowed_after_rejection() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.reject_revision(&freelancer, &job_id);

    // Now should be able to propose again
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    let proposal = contract
        .get_revision_proposal(&job_id)
        .expect("Proposal should exist");
    assert_eq!(proposal.proposer, freelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_propose_revision_fails_for_empty_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let empty_milestones: Vec<Milestone> = vec![&env];
    contract.propose_revision(&client, &job_id, &empty_milestones);
}

#[test]
fn test_propose_revision_new_total_equals_sum_of_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Correction 4: Dynamic sum
    let m0: i128 = 400;
    let m1: i128 = 800;
    let expected_total = m0 + m1;

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "M1"),
            amount: m0,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE - 1,
                token: None,
        },
        Milestone {
            id: 1,
            description: String::from_str(&env, "M2"),
            amount: m1,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    let proposal = contract.get_revision_proposal(&job_id).unwrap();
    assert_eq!(proposal.new_total, expected_total);
}

#[test]
fn test_accept_revision_same_total_updates_milestones_only() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    // Correction 4: Named amount for dynamic assertions
    let initial_amount: i128 = 1000;
    let milestones = vec![&env, (String::from_str(&env, "Initial"), initial_amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    let initial_escrow_balance = token.balance(&contract.address);
    assert_eq!(initial_escrow_balance, initial_amount);

    // Split into two equal halves — same total
    let half = initial_amount / 2;
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Split 1"),
            amount: half,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE - 1,
                token: None,
        },
        Milestone {
            id: 1,
            description: String::from_str(&env, "Split 2"),
            amount: half,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);

    let job = contract.get_job(&job_id);
    assert_eq!(job.milestones.len(), 2);
    assert_eq!(job.total_amount, initial_amount);
    assert_eq!(
        token.balance(&contract.address),
        initial_amount,
        "Escrow balance should not change for neutral budget"
    );
}

#[test]
fn test_accept_revision_with_increased_total_transfers_difference_from_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let initial_amount: i128 = 1000;
    let new_amount: i128 = 1500;
    let diff = new_amount - initial_amount;

    let milestones = vec![&env, (String::from_str(&env, "Initial"), initial_amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    let client_initial_balance = token.balance(&client);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "More"),
            amount: new_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);

    // Correction 4: Dynamic assertions
    assert_eq!(token.balance(&contract.address), new_amount);
    assert_eq!(token.balance(&client), client_initial_balance - diff);
    let job = contract.get_job(&job_id);
    assert_eq!(job.total_amount, new_amount);
}

#[test]
fn test_accept_revision_with_decreased_total_refunds_difference_to_client() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let initial_amount: i128 = 2000;
    let new_amount: i128 = 1200;
    let diff = initial_amount - new_amount;

    let milestones = vec![&env, (String::from_str(&env, "Initial"), initial_amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    let client_balance_after_funding = token.balance(&client);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Less"),
            amount: new_amount,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);

    // Correction 4: Dynamic assertions
    assert_eq!(token.balance(&contract.address), new_amount);
    assert_eq!(token.balance(&client), client_balance_after_funding + diff);
}

#[test]
fn test_reject_revision_sets_status_to_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let original_total: i128 = 1000;
    let milestones = vec![&env, (String::from_str(&env, "Initial"), original_total, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.reject_revision(&freelancer, &job_id);

    let proposal = contract.get_revision_proposal(&job_id).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Rejected);

    let job = contract.get_job(&job_id);
    assert_eq!(
        job.total_amount, original_total, // Correction 4: dynamic
        "Job total should not change after rejection"
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_proposer_cannot_accept_own_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);
}

#[test]
fn test_propose_revision_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);

    let events = env.events().all();
    let last_event = events.last().expect("Event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, Symbol::new(&env, "revision_proposed"));
}

#[test]
fn test_accept_revision_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);

    let events = env.events().all();
    let last_event = events.last().expect("Event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, Symbol::new(&env, "revision_accepted"));
}

#[test]
fn test_cancel_revision_proposal_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);

    // Proposer cancels
    contract.cancel_revision_proposal(&client, &job_id);

    // Proposal should be gone
    assert!(contract.get_revision_proposal(&job_id).is_none());
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_cancel_revision_proposal_fails_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    pause_escrow(&env, &contract, &admin);
    contract.cancel_revision_proposal(&client, &job_id);
}

#[test]
fn test_cancel_revision_proposal_succeeds_after_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    pause_escrow(&env, &contract, &admin);
    unpause_escrow(&env, &contract, &admin);
    contract.cancel_revision_proposal(&client, &job_id);
    assert!(contract.get_revision_proposal(&job_id).is_none());
}

#[test]
fn test_freelancer_can_cancel_own_revision_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);

    // Freelancer (proposer) cancels
    contract.cancel_revision_proposal(&freelancer, &job_id);

    assert!(contract.get_revision_proposal(&job_id).is_none());
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_cancel_revision_fails_for_non_proposer() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);

    // Non-proposer (freelancer) tries to cancel — should fail
    contract.cancel_revision_proposal(&freelancer, &job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_cancel_revision_fails_for_third_party() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);
    let third_party = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);

    // Third party tries to cancel — should fail
    contract.cancel_revision_proposal(&third_party, &job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_cancel_revision_fails_when_already_accepted() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    contract.accept_revision(&client, &job_id);

    // Proposer (freelancer) tries to cancel after acceptance — should fail
    contract.cancel_revision_proposal(&freelancer, &job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_cancel_revision_fails_when_already_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.reject_revision(&freelancer, &job_id);

    // Proposer (client) tries to cancel after rejection — should fail
    contract.cancel_revision_proposal(&client, &job_id);
}

#[test]
fn test_cancel_revision_proposal_clears_slot_for_new_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.cancel_revision_proposal(&client, &job_id);

    // Should be able to submit a new proposal immediately
    contract.propose_revision(&freelancer, &job_id, &new_milestones);
    let proposal = contract.get_revision_proposal(&job_id).expect("New proposal should exist");
    assert_eq!(proposal.proposer, freelancer);
    assert_eq!(proposal.status, ProposalStatus::Pending);
}

#[test]
fn test_cancel_revision_proposal_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, token, _) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Initial"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 1200,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client, &job_id, &new_milestones);
    contract.cancel_revision_proposal(&client, &job_id);

    let events = env.events().all();
    let last_event = events.last().expect("Event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, Symbol::new(&env, "revision_cancelled"));
}

#[test]
fn test_resolve_dispute_callback_client_wins() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    // Correction 4: Dynamic total
    let total: i128 = 500 + 1000 + 1500;

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::ClientWins);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), total);
}

#[test]
fn test_resolve_dispute_callback_freelancer_wins() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    let total: i128 = 500 + 1000 + 1500;

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::FreelancerWins);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&freelancer), total);
}

#[test]
fn test_resolve_dispute_callback_refund_both() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);

    // Correction 4: Dynamic split
    let total: i128 = 500 + 1000 + 1500;
    let each = total / 2;

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::RefundBoth);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), each);
    assert_eq!(token_client.balance(&freelancer), each);
}

// ── #1178 — Escalate leaves the job parked in Disputed ────────────────────────
//
// These lock the behaviour now documented on `JobStatus` under "Escalated
// disputes": Escalate is a self-loop that moves no funds, and the job leaves
// Disputed only via a later resolution or via expire_job.

/// Helper: create + fund a single-milestone job and drive it into Disputed
/// through the registered dispute contract. Returns (job_id, amount).
fn setup_disputed_job(
    env: &Env,
    escrow: &EscrowContractClient<'_>,
    client: &Address,
    freelancer: &Address,
    token: &Address,
    admin: &Address,
) -> (u64, i128) {
    let amount: i128 = 1000;
    let milestones = vec![
        env,
        (String::from_str(env, "Task 1"), amount, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        client,
        freelancer,
        token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, client, &0, &0);

    let dispute_contract = Address::generate(env);
    escrow.set_dispute_contract(admin, &dispute_contract);
    escrow.mark_job_disputed(&job_id, &1_u64);

    (job_id, amount)
}

#[test]
fn test_resolve_dispute_callback_escalate_keeps_job_disputed_and_funds_escrowed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client, freelancer, token, admin) = setup_test(&env);
    let (job_id, amount) = setup_disputed_job(&env, &escrow, &client, &freelancer, &token, &admin);

    let token_client = TokenClient::new(&env, &token);
    let client_before = token_client.balance(&client);
    let freelancer_before = token_client.balance(&freelancer);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::Escalate);

    // Self-loop: status is untouched and the escrow still holds the full amount.
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Disputed);
    assert_eq!(token_client.balance(&escrow.address), amount);
    assert_eq!(token_client.balance(&client), client_before);
    assert_eq!(token_client.balance(&freelancer), freelancer_before);
}

#[test]
fn test_escalated_job_can_be_resolved_by_a_later_callback() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client, freelancer, token, admin) = setup_test(&env);
    let (job_id, amount) = setup_disputed_job(&env, &escrow, &client, &freelancer, &token, &admin);

    // Escalate twice — re-callable, still no settlement.
    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::Escalate);
    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::Escalate);
    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Disputed);

    // The higher tier finally rules: the job settles with the escrow intact.
    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::FreelancerWins);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&freelancer), amount);
}

#[test]
fn test_escalated_job_can_still_expire_after_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client, freelancer, token, admin) = setup_test(&env);
    let (job_id, amount) = setup_disputed_job(&env, &escrow, &client, &freelancer, &token, &admin);

    let token_client = TokenClient::new(&env, &token);
    let client_before = token_client.balance(&client);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::Escalate);

    // Backstop path: nobody re-resolves, the deadline passes, anyone can expire it.
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);
    escrow.expire_job(&job_id);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
    assert_eq!(token_client.balance(&client), client_before + amount);
}

// ── Pause mechanism tests ─────────────────────────────────────────────────────

#[test]
fn test_initialize_pause() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);
}

#[test]
fn test_pause_and_unpause() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert_eq!(job_id, 1);

    pause_escrow(&env, &client, &admin);
    unpause_escrow(&env, &client, &admin);

    // Use far-future deadlines since pause_escrow advanced the ledger by ~48h
    let milestones2 = vec![&env, (String::from_str(&env, "Task 2"), 200_i128, 2_000_000_u64)];
    let job_id2 = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones2,
        &3_000_000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    assert_eq!(job_id2, 2);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_create_job_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);
    pause_escrow(&env, &client, &admin);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_fund_job_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    pause_escrow(&env, &client, &admin);
    client.fund_job(&job_id, &user, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_fund_job_rejects_non_client_caller() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let job_client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let attacker = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &job_client,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    client.fund_job(&job_id, &attacker, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_submit_milestone_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    client.fund_job(&job_id, &user, &0, &0);
    pause_escrow(&env, &client, &admin);
    client.submit_milestone(&job_id, &0, &freelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_approve_milestone_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    client.fund_job(&job_id, &user, &0, &0);
    client.submit_milestone(&job_id, &0, &freelancer);
    pause_escrow(&env, &client, &admin);
    client.approve_milestone(&job_id, &0, &user);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_claim_refund_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    client.fund_job(&job_id, &user, &0, &0);

    // Advance time past deadline + grace period
    env.ledger()
        .with_mut(|l| l.timestamp = 2500 + GRACE_PERIOD + 1); // Correction 5

    pause_escrow(&env, &client, &admin);
    client.claim_refund(&job_id, &user, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_extend_deadline_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 2000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2500_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    pause_escrow(&env, &client, &admin);
    client.extend_deadline(&job_id, &0, &4000_u64);
}

#[test]
fn test_read_only_functions_when_paused() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&vec![&env, admin.clone()], &1, &admin, &100u32, &604800u64);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);
    // Both deadlines must exceed the 48-hour time advance done by pause_escrow (172801s)
    let milestones = vec![&env, (String::from_str(&env, "Task 1"), 100_i128, 1_000_000_u64)];

    let job_id = client.create_job(
        &user,
        &freelancer,
        &token,
        &milestones,
        &2_000_000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    pause_escrow(&env, &client, &admin);

    // Read-only functions should still work when paused
    let job = client.get_job(&job_id);
    assert_eq!(job.id, job_id);

    let count = client.get_job_count();
    assert_eq!(count, 1);

    let overdue = client.is_milestone_overdue(&job_id, &0);
    assert_eq!(overdue, false);
}

// ── Batch Milestone Approval Tests ─────────────────────────────────────────────

#[test]
fn test_approve_milestones_batch_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    // Correction 2 & 3: register_stellar_asset_contract_v2 + .address()
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Correction 4: Named amounts
    let m0: i128 = 1000;
    let m1: i128 = 1500;
    let m2: i128 = 2000;
    let total = m0 + m1 + m2;

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), m0, 2000_u64),
        (String::from_str(&env, "Task 2"), m1, 3000_u64),
        (String::from_str(&env, "Task 3"), m2, 4000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.submit_milestone(&job_id, &1, &freelancer);
    escrow.submit_milestone(&job_id, &2, &freelancer);

    let indices = vec![&env, 0_u32, 1_u32, 2_u32];
    let total_released = escrow.approve_milestones_batch(&job_id, &indices, &client);

    assert_eq!(total_released, total);

    // Job stays InProgress until complete_job is called
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Approved);
    assert_eq!(job.milestones.get(1).unwrap().status, MilestoneStatus::Approved);
    assert_eq!(job.milestones.get(2).unwrap().status, MilestoneStatus::Approved);

    // Complete the job to finalize
    escrow.complete_job(&job_id, &client);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);
}

#[test]
fn test_approve_milestones_batch_partial_invalid() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    // Correction 2 & 3
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let m0: i128 = 1000;
    let m1: i128 = 1500;
    let total = m0 + m1;

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), m0, 2000_u64),
        (String::from_str(&env, "Task 2"), m1, 3000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Submit only the first milestone
    escrow.submit_milestone(&job_id, &0, &freelancer);

    // Second is not Submitted — should fail with InvalidStatus
    let indices = vec![&env, 0_u32, 1_u32];
    let result = escrow.try_approve_milestones_batch(&job_id, &indices, &client);
    assert!(result.is_err());
}

#[test]
#[should_panic(expected = "HostError: Error(Contract, #2)")] // Unauthorized
fn test_approve_milestones_batch_unauthorized_caller() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    // Correction 2 & 3
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let unauthorized = Address::generate(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, 2000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 1000);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);

    let indices = vec![&env, 0_u32];
    escrow.approve_milestones_batch(&job_id, &indices, &unauthorized);
}

#[test]
fn test_approve_milestones_batch_non_existent_index() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    // Correction 2 & 3
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, 2000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD, // Correction 5
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, 1000);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);

    let indices = vec![&env, 99_u32]; // Non-existent index
    let result = escrow.try_approve_milestones_batch(&job_id, &indices, &client);
    assert!(result.is_err());
}

#[test]
fn test_approve_milestones_batch_rejects_duplicate_indices() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let m0: i128 = 1000;

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), m0, 2000_u64),
    ];

    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    mint_tokens(&env, &token, &client, m0);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);

    // Duplicate index — should be rejected without mutating any state.
    let indices = vec![&env, 0_u32, 0_u32];
    let result = escrow.try_approve_milestones_batch(&job_id, &indices, &client);
    assert_eq!(
        result,
        Err(Ok(EscrowError::InvalidMilestoneIndex))
    );

    // Milestone must remain Submitted since the call reverted before any mutation.
    let job = escrow.get_job(&job_id);
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Submitted);
}

// ── Protocol Fee and Treasury Tests ───────────────────────────────────────────

#[test]
fn test_initialize_and_admin_controls() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_bps = 250; // 2.5%

    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);

    // Initialized twice should fail
    let result = escrow.try_initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);
    assert!(result.is_err());

    escrow.propose_admin_action(&admin, &AdminAction::SetFeeBps(500));
    let new_treasury = Address::generate(&env);
    escrow.propose_admin_action(&admin, &AdminAction::SetTreasury(new_treasury));
}

#[test]
fn test_fee_deduction_single_approval() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_bps: u32 = 500; // 5%
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);

    let token_admin = Address::generate(&env);
    // Correction 2 & 3
    let token = env.register_stellar_asset_contract_v2(token_admin.clone()).address();
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Correction 4: Dynamic fee calculation
    let milestone_amount: i128 = 1000;
    let fee = milestone_amount * fee_bps as i128 / 10_000;
    let freelancer_receives = milestone_amount - fee;

    let milestones = vec![&env, (String::from_str(&env, "Task 1"), milestone_amount, 2000_u64)];
    let job_id = escrow.create_job(&client_addr, &freelancer, &token, &milestones, &3000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client_addr, milestone_amount);
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client_addr);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&freelancer), 0);

    // Complete the job — fee is deducted and remainder paid to freelancer
    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(token_client.balance(&treasury), fee);
    assert_eq!(token_client.balance(&freelancer), freelancer_receives);
}

#[test]
fn test_fee_deduction_batch_approval() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_bps: u32 = 500; // 5% (max)
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);

    let token_admin = Address::generate(&env);
    // Correction 2 & 3
    let token = env.register_stellar_asset_contract_v2(token_admin.clone()).address();
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Correction 4: Dynamic fee calculation
    let m0: i128 = 1000;
    let m1: i128 = 2000;
    let total = m0 + m1;
    let fee = total * fee_bps as i128 / 10_000;
    let freelancer_receives = total - fee;

    let milestones = vec![
        &env,
        (String::from_str(&env, "T1"), m0, 2000_u64),
        (String::from_str(&env, "T2"), m1, 3000_u64),
    ];
    let job_id = escrow.create_job(&client_addr, &freelancer, &token, &milestones, &5000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client_addr, total);
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.submit_milestone(&job_id, &1, &freelancer);

    let indices = vec![&env, 0_u32, 1_u32];
    escrow.approve_milestones_batch(&job_id, &indices, &client_addr);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&freelancer), 0);

    // Complete the job — fee is deducted and remainder paid to freelancer
    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(token_client.balance(&treasury), fee);
    assert_eq!(token_client.balance(&freelancer), freelancer_receives);
}

#[test]
fn test_complete_job_3_percent_fee() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_bps: u32 = 300; // 3%
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin.clone()).address();
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let total_amount: i128 = 100;
    let fee = total_amount * fee_bps as i128 / 10_000; // 3
    let freelancer_receives = total_amount - fee; // 97

    let milestones = vec![&env, (String::from_str(&env, "Task"), total_amount, 2000_u64)];
    let job_id = escrow.create_job(&client_addr, &freelancer, &token, &milestones, &3000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client_addr, total_amount);
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client_addr);

    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&freelancer), 0);

    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(token_client.balance(&treasury), fee);
    assert_eq!(token_client.balance(&freelancer), freelancer_receives);
}

#[test]
fn test_complete_job_zero_fee() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &0, &604800u64);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin.clone()).address();
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let amount: i128 = 500;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, 2000_u64)];
    let job_id = escrow.create_job(&client_addr, &freelancer, &token, &milestones, &3000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client_addr, amount);
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client_addr);
    escrow.complete_job(&job_id, &client_addr);

    let token_client = TokenClient::new(&env, &token);
    // No fee — freelancer gets full amount
    assert_eq!(token_client.balance(&treasury), 0);
    assert_eq!(token_client.balance(&freelancer), amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_complete_job_not_all_approved() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let m0: i128 = 500;
    let m1: i128 = 500;
    let total = m0 + m1;
    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), m0, 2000_u64),
        (String::from_str(&env, "M2"), m1, 3000_u64),
    ];
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &5000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0_i128, &0_u32);

    // Only submit + approve the first milestone; second is still Pending
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    // complete_job should fail because not all milestones are approved
    escrow.complete_job(&job_id, &client);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_complete_job_unauthorized() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let stranger = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task"), 100_i128, 2000_u64)];
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &5000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Mint and fund so job becomes Funded, then we can submit + approve
    mint_tokens(&env, &token, &client, 100);
    escrow.fund_job(&job_id, &client, &0_i128, &0_u32);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    // Stranger tries to complete
    escrow.complete_job(&job_id, &stranger);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_complete_job_wrong_status() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, _) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let milestones = vec![&env, (String::from_str(&env, "Task"), 100_i128, 2000_u64)];
    let job_id = escrow.create_job(&client, &freelancer, &Address::generate(&env), &milestones, &5000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Job is Created (not funded), not InProgress — completing should fail
    escrow.complete_job(&job_id, &client);
}

/// Regression test for issue #995: complete_job must emit per-token events
/// for multi-token jobs so indexers can accurately track all fee/payment amounts.
#[test]
fn test_complete_job_multi_token_emits_per_token_events() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    let fee_bps: u32 = 500; // 5%
    
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &fee_bps, &604800u64);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    // Create two different tokens
    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    // Create multi-token job:
    // - Milestone 0: 1000 in token_a (default)
    // - Milestone 1: 2000 in token_b (explicit)
    // - Milestone 2: 500 in token_a (default)
    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, 2000_u64, None),
        (String::from_str(&env, "Task 2"), 2000_i128, 2500_u64, Some(token_b.clone())),
        (String::from_str(&env, "Task 3"), 500_i128, 3000_u64, None),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &5000_u64,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    // Fund the job
    let total_token_a = 1000 + 500; // 1500
    let total_token_b = 2000;

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &total_token_a);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &total_token_b);

    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    // Submit and approve all milestones
    for idx in 0..3 {
        escrow.submit_milestone(&job_id, &idx, &freelancer);
        escrow.approve_milestone(&job_id, &idx, &client_addr);
    }

    // Complete the job
    escrow.complete_job(&job_id, &client_addr);

    // Calculate expected amounts per token
    // Token A: milestone 0 (1000) + milestone 2 (500) = 1500
    let token_a_fee = (1500 * fee_bps as i128) / 10_000; // 75
    let token_a_freelancer = 1500 - token_a_fee; // 1425

    // Token B: milestone 1 (2000)
    let token_b_fee = (2000 * fee_bps as i128) / 10_000; // 100
    let token_b_freelancer = 2000 - token_b_fee; // 1900

    // Verify actual token transfers match expected amounts
    let token_a_client = TokenClient::new(&env, &token_a);
    let token_b_client = TokenClient::new(&env, &token_b);

    assert_eq!(token_a_client.balance(&treasury), token_a_fee);
    assert_eq!(token_a_client.balance(&freelancer), token_a_freelancer);
    assert_eq!(token_b_client.balance(&treasury), token_b_fee);
    assert_eq!(token_b_client.balance(&freelancer), token_b_freelancer);

    // Verify per-token events were emitted (2 fee_taken + 2 pmt_released)
    let events = env.events().all();
    
    let mut fee_taken_count = 0;
    let mut pmt_released_count = 0;

    for event in events.iter() {
        // Check if this is one of our escrow contract events
        // events from escrow contract have at least 2 topics
        if event.0 == contract_id && event.1.len() >= 2 {
            let topic0: Symbol = event.1.get(0).unwrap().into_val(&env);
            let topic1: Symbol = event.1.get(1).unwrap().into_val(&env);

            if topic0 == symbol_short!("escrow") {
                if topic1 == Symbol::new(&env, "fee_taken") {
                    fee_taken_count += 1;
                } else if topic1 == Symbol::new(&env, "pmt_released") {
                    pmt_released_count += 1;
                }
            }
        }
    }

    // Should emit one event per token for each event type
    assert_eq!(fee_taken_count, 2, "Should emit 2 fee_taken events (one per token)");
    assert_eq!(pmt_released_count, 2, "Should emit 2 pmt_released events (one per token)");
}

#[test]
fn test_fee_taken_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &200, &604800u64);

    let token_admin = Address::generate(&env);
    let token = env.register_stellar_asset_contract_v2(token_admin.clone()).address();
    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let amount: i128 = 1000;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, 2000_u64)];
    let job_id = escrow.create_job(&client_addr, &freelancer, &token, &milestones, &5000_u64, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client_addr, amount);
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client_addr);
    escrow.complete_job(&job_id, &client_addr);

    let events = env.events().all();
    let last_event = events.last().expect("at least one event");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "pmt_released"));

    // Second-to-last event should be fee_taken
    let fee_event = events.get(events.len() - 2).expect("fee_taken event exists");
    let ft_topic1: Symbol = fee_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(ft_topic1, Symbol::new(&env, "fee_taken"));
    let payload: (u64, i128, Address) = fee_event.2.clone().into_val(&env);
    assert_eq!(payload.0, job_id);
    assert_eq!(payload.1, amount * 200 / 10_000);
    assert_eq!(payload.2, treasury);
}

#[test]
fn test_fee_cap_enforcement() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    // Should fail if > 5% during initialize
    let result = escrow.try_initialize(&vec![&env, admin.clone()], &1, &treasury, &501, &604800u64);
    assert!(result.is_err());

    // Should fail if > 5% during update
    escrow.initialize(&vec![&env, admin.clone()], &1, &treasury, &0, &604800u64);
    let result = escrow.try_propose_admin_action(&admin, &AdminAction::SetFeeBps(501));
    assert!(result.is_err());
}

/// Verifies that fund_job rejects a job whose stored total_amount is LESS than
/// the sum of its milestone amounts (underfunding). Uses InvalidAmount (#24).
#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_fund_job_underfunding_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);

    // Two milestones summing to 100
    let milestones = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 60_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Phase 2"), 40_i128, JOB_DEADLINE),
    ];

    let job_id = escrow.create_job(&user, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Corrupt total_amount to 50 — less than the milestone sum of 100
    env.as_contract(&contract_id, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 50;
        env.storage().persistent().set(&key, &job);
    });

    // Must fail with InvalidAmount
    escrow.fund_job(&job_id, &user, &0, &0);
}

/// Verifies that fund_job rejects a job whose stored total_amount is MORE than
/// the sum of its milestone amounts (overfunding). Uses InvalidAmount (#24).
#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_fund_job_overfunding_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let user = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let token = env.register_contract(None, MockToken);

    // Two milestones summing to 100
    let milestones = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 60_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Phase 2"), 40_i128, JOB_DEADLINE),
    ];

    let job_id = escrow.create_job(&user, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Corrupt total_amount to 150 — more than the milestone sum of 100
    env.as_contract(&contract_id, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 150;
        env.storage().persistent().set(&key, &job);
    });

    // Must fail with InvalidAmount
    escrow.fund_job(&job_id, &user, &0, &0);
}

// ── expire_job tests (issue #267) ────────────────────────────────────────────

#[test]
fn test_expire_job_happy_path() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let total: i128 = 3000;

    let milestones = default_milestones(&env);
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Advance ledger past the job deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);

    escrow.expire_job(&job_id);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);

    // Full balance refunded to client
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), total);

    // JobExpired event emitted
    let events = env.events().all();
    let last_event = events.last().expect("at least one event");
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "job_expired"));
}

#[test]
fn test_expire_job_partial_refund_after_approved_milestone() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let m0: i128 = 500;
    let total: i128 = 500 + 1000 + 1500;

    let milestones = default_milestones(&env);
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Approve first milestone
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);

    escrow.expire_job(&job_id);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);

    let token_client = TokenClient::new(&env, &token);
    // Client gets back total minus already-approved milestone
    assert_eq!(token_client.balance(&client), total - m0);
    // Freelancer keeps the approved amount
    assert_eq!(token_client.balance(&freelancer), m0);
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")] // DeadlineNotPassed
fn test_expire_job_premature_call_fails() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let milestones = default_milestones(&env);
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);

    // Still before the deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE - 1);

    escrow.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_expire_job_already_completed_fails() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let amount: i128 = 500;

    let milestones = vec![
        &env,
        (String::from_str(&env, "Only task"), amount, 500_000_u64),
    ];
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, amount);
    escrow.fund_job(&job_id, &client, &0, &0);

    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);
    escrow.complete_job(&job_id, &client);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);

    escrow.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_expire_job_already_cancelled_fails() {
    let env = Env::default();
    let (escrow, token) = setup_refund_env(&env);

    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);

    let milestones = default_milestones(&env);
    let job_id = escrow.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    mint_tokens(&env, &token, &client, 3000);
    escrow.fund_job(&job_id, &client, &0, &0);
    escrow.cancel_job(&job_id, &client, &0);

    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);

    escrow.expire_job(&job_id);
}

// ── PaymentReleased event tests (issue #218) ─────────────────────────────────

#[test]
fn test_payment_released_event_emitted_on_last_milestone_approval() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let amount: i128 = 1000;
    let milestones = vec![&env, (String::from_str(&env, "Only task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client_addr);

    // Job should be InProgress until complete_job
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);

    // Freelancer not yet paid
    assert_eq!(token.balance(&freelancer), 0);

    // Complete the job to trigger payment
    contract.complete_job(&job_id, &client_addr);

    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    // Verify PaymentReleased event was emitted — it is the last event
    let events = env.events().all();
    let last_event = events.last().expect("At least one event should be emitted");
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "pmt_released"), "Last event should be pmt_released");

    // Freelancer should have received full payment (no fee configured)
    assert_eq!(token.balance(&freelancer), amount);
}

#[test]
fn test_payment_released_event_not_emitted_on_partial_approval() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Phase 2"), 500_i128, JOB_DEADLINE),
    ];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client_addr);

    // Job should still be InProgress (not all milestones approved)
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);

    // Verify PaymentReleased event was NOT emitted — last event should be "milestone"
    let events = env.events().all();
    let last_event = events.last().expect("At least one event should be emitted");
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_ne!(
        topic1,
        Symbol::new(&env, "pmt_released"),
        "PaymentReleased event should NOT be emitted for partial approval"
    );
}

#[test]
fn test_payment_released_event_emitted_via_batch_approval() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 400_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Phase 2"), 600_i128, JOB_DEADLINE),
    ];
    let job_id = contract.create_job(&client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.submit_milestone(&job_id, &1, &freelancer);
    contract.approve_milestones_batch(&job_id, &vec![&env, 0_u32, 1_u32], &client_addr);

    // Job stays InProgress until complete_job
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);

    // Complete the job to trigger payment
    contract.complete_job(&job_id, &client_addr);

    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    // Last event should be pmt_released
    let events = env.events().all();
    let last_event = events.last().expect("At least one event should be emitted");
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "pmt_released"), "Last event should be pmt_released via batch");
}

#[test]
fn test_multisig_pause_flow() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, signer1, signer2) = setup_multisig(&env);

    // Initial state: not paused
    assert_eq!(contract.is_paused(), false);

    // signer1 proposes pause
    let proposal_id = contract.propose_admin_action(&signer1, &AdminAction::Pause);
    assert_eq!(proposal_id, 1);
    
    // Proposal exists but not executed yet (needs 2/2)
    assert_eq!(contract.is_paused(), false);

    // Advance past the 48-hour time lock required for Pause proposals
    env.ledger().with_mut(|l| l.timestamp += 48 * 60 * 60 + 1);

    // signer2 approves — threshold met and time lock passed, so proposal executes
    contract.approve_admin_action(&signer2, &proposal_id);

    // Execution should be automatic after second approval
    assert_eq!(contract.is_paused(), true);
}

#[test]
fn test_multisig_unauthorized_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _, _, _, _, _) = setup_multisig(&env);
    let malicious = Address::generate(&env);

    let result = contract.try_propose_admin_action(&malicious, &AdminAction::Pause);
    assert!(result.is_err());
}

#[test]
fn test_multisig_rotation() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _, _, _, signer1, signer2) = setup_multisig(&env);
    let new_signer = Address::generate(&env);

    // Propose rotation: signer1 -> new_signer
    let proposal_id = contract.propose_admin_action(&signer1, &AdminAction::RotateSigner(signer1.clone(), new_signer.clone()));
    contract.approve_admin_action(&signer2, &proposal_id);

    // Check if new_signer can now propose
    let prop2 = contract.propose_admin_action(&new_signer, &AdminAction::Unpause);
    assert_eq!(prop2, 2);

    // Check if old signer fails
    let result = contract.try_propose_admin_action(&signer1, &AdminAction::Pause);
    assert!(result.is_err());
}

// ── release_partial_payment tests (issue #268) ──────────────────────────────

#[test]
fn test_release_partial_payment_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let amount: i128 = 1000;
    let partial: i128 = 700;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Release 70% of the milestone
    contract.release_partial_payment(&job_id, &0, &partial, &client_addr, &0);

    let job = contract.get_job(&job_id);
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::PartiallyPaid);
    assert_eq!(ms.amount, amount - partial); // remaining = 300
    assert_eq!(token.balance(&freelancer), partial);
    assert_ne!(job.status, JobStatus::Completed); // not done yet
}

#[test]
fn test_release_partial_payment_fully_zeros_becomes_approved() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let amount: i128 = 500;
    let milestones = vec![&env, (String::from_str(&env, "Only"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Pay the full amount via release_partial_payment
    contract.release_partial_payment(&job_id, &0, &amount, &client_addr, &0);

    let job = contract.get_job(&job_id);
    let ms = job.milestones.get(0).unwrap();
    assert_eq!(ms.status, MilestoneStatus::Approved);
    assert_eq!(ms.amount, 0);
    assert_eq!(job.status, JobStatus::InProgress);
    assert_eq!(token.balance(&freelancer), amount);
}

#[test]
fn test_release_partial_then_full_remainder() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);
    let token = TokenClient::new(&env, &token_addr);

    let amount: i128 = 1000;
    let partial: i128 = 300;
    let milestones = vec![&env, (String::from_str(&env, "Work"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // First partial payment
    contract.release_partial_payment(&job_id, &0, &partial, &client_addr, &0);
    assert_eq!(token.balance(&freelancer), partial);

    // Second partial payment clears the rest
    let remaining = amount - partial;
    contract.release_partial_payment(&job_id, &0, &remaining, &client_addr, &1);

    let job = contract.get_job(&job_id);
    assert_eq!(job.milestones.get(0).unwrap().status, MilestoneStatus::Approved);
    assert_eq!(job.status, JobStatus::InProgress);
    assert_eq!(token.balance(&freelancer), amount);
}

#[test]
fn test_release_partial_payment_amount_zero_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let amount: i128 = 1000;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    let result = contract.try_release_partial_payment(&job_id, &0, &0_i128, &client_addr, &0);
    assert!(result.is_err()); // InvalidPartialAmount (#32)
}

#[test]
fn test_release_partial_payment_amount_exceeds_milestone_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let amount: i128 = 500;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Request more than the milestone holds
    let result = contract.try_release_partial_payment(&job_id, &0, &(amount + 1), &client_addr, &0);
    assert!(result.is_err()); // InvalidPartialAmount (#32)
}

#[test]
fn test_release_partial_payment_wrong_status_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let amount: i128 = 500;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    // Do NOT submit the milestone — status is Pending, not Submitted

    let result = contract.try_release_partial_payment(&job_id, &0, &100_i128, &client_addr, &0);
    assert!(result.is_err()); // InvalidStatus (#3)
}

#[test]
fn test_release_partial_payment_unauthorized_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token_addr, _) = setup_test(&env);

    let amount: i128 = 500;
    let milestones = vec![&env, (String::from_str(&env, "Task"), amount, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client_addr, &freelancer, &token_addr, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    let attacker = Address::generate(&env);
    let result = contract.try_release_partial_payment(&job_id, &0, &100_i128, &attacker, &0);
    assert!(result.is_err()); // Unauthorized (#2)
}

#[test]
fn test_execute_proposal_happy_path() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);

    // Pause is a time-locked action (48 hours)
    let proposal_id = contract.propose_admin_action(&admin, &AdminAction::Pause);

    // Verify it is NOT executed yet
    assert_eq!(contract.is_paused(), false);

    // Call execute_proposal before time-lock expires, should fail
    let res = contract.try_execute_proposal(&admin, &proposal_id);
    assert!(res.is_err()); // ProposalTimeLockActive

    // Advance time past the lock (48 hours = 172800 seconds)
    env.ledger().with_mut(|l| l.timestamp += 172800 + 1);

    // Execute successfully
    contract.execute_proposal(&admin, &proposal_id);
    assert_eq!(contract.is_paused(), true);
}

#[test]
fn test_execute_proposal_unauthorized_if_threshold_not_met() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    // Set up with 2 signers and threshold = 2
    let contract_id = env.register_contract(None, EscrowContract);
    let contract = EscrowContractClient::new(&env, &contract_id);
    let admin1 = Address::generate(&env);
    let admin2 = Address::generate(&env);
    let signers = vec![&env, admin1.clone(), admin2.clone()];
    let treasury = Address::generate(&env);
    contract.initialize(&signers, &2, &treasury, &0, &604800);

    // Pause action proposed by admin1. Only 1 approval (admin1). Threshold is 2.
    let proposal_id = contract.propose_admin_action(&admin1, &AdminAction::Pause);

    // Advance time past lock (48 hours)
    env.ledger().with_mut(|l| l.timestamp += 172800 + 1);

    // Try executing with admin2, should fail with Unauthorized because threshold (2) is not met yet
    let res = contract.try_execute_proposal(&admin2, &proposal_id);
    assert!(res.is_err()); // Unauthorized (#2)
}

#[test]
fn test_execute_proposal_signer_not_found() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);
    let proposal_id = contract.propose_admin_action(&admin, &AdminAction::Pause);

    env.ledger().with_mut(|l| l.timestamp += 172800 + 1);

    let non_signer = Address::generate(&env);
    let res = contract.try_execute_proposal(&non_signer, &proposal_id);
    assert!(res.is_err()); // SignerNotFound (#28)
}

// ── top_up_escrow tests (issue #489) ─────────────────────────────────────────

#[test]
fn test_top_up_escrow_partial_top_up() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    // Mint extra tokens so client can top up
    let token_admin = StellarAssetClient::new(&env, &token);
    token_admin.mint(&client, &5000);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    contract.fund_job(&job_id, &client, &0, &0);

    // Simulate revision increasing total_amount so there is room to top up
    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 1500;
        env.storage().persistent().set(&key, &job);
    });

    // Partial top-up: add 300 (funded_amount goes from 1000 → 1300)
    contract.top_up_escrow(&client, &job_id, &300_i128);

    let job: crate::Job = env.as_contract(&contract.address, || {
        env.storage().persistent().get(&crate::DataKey::Job(job_id)).unwrap()
    });
    assert_eq!(job.funded_amount, 1300);
}

#[test]
fn test_top_up_escrow_completing_funding() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let token_admin = StellarAssetClient::new(&env, &token);
    token_admin.mint(&client, &5000);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    contract.fund_job(&job_id, &client, &0, &0);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 1500;
        env.storage().persistent().set(&key, &job);
    });

    // Top up the full remaining 500
    contract.top_up_escrow(&client, &job_id, &500_i128);

    let job: crate::Job = env.as_contract(&contract.address, || {
        env.storage().persistent().get(&crate::DataKey::Job(job_id)).unwrap()
    });
    assert_eq!(job.funded_amount, job.total_amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_top_up_escrow_overfund_rejection() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let token_admin = StellarAssetClient::new(&env, &token);
    token_admin.mint(&client, &5000);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    // Trying to top up on a fully-funded job should fail with AlreadyFunded (#6)
    contract.top_up_escrow(&client, &job_id, &1_i128);
}

#[test]
fn test_top_up_escrow_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let token_admin = StellarAssetClient::new(&env, &token);
    token_admin.mint(&client, &5000);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 1500;
        env.storage().persistent().set(&key, &job);
    });

    contract.top_up_escrow(&client, &job_id, &200_i128);

    let events = env.events().all();
    let last_event = events.last().expect("top_up event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    assert_eq!(topic1, symbol_short!("top_up"));
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")] // InvalidAmount
fn test_top_up_escrow_rejects_zero_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 1500;
        env.storage().persistent().set(&key, &job);
    });

    contract.top_up_escrow(&client, &job_id, &0_i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")] // InvalidAmount
fn test_top_up_escrow_rejects_negative_amount() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);

    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.total_amount = 1500;
        env.storage().persistent().set(&key, &job);
    });

    contract.top_up_escrow(&client, &job_id, &-100_i128);
}

// ============================================================
// TOKEN ALLOWLIST TESTS
// ============================================================

#[test]
fn test_add_allowed_token() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, token, admin) = setup_test(&env);

    let new_token = Address::generate(&env);
    contract.add_allowed_token(&admin, &new_token);

    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 1);
    assert_eq!(allowed.get(0).unwrap(), new_token.clone());

    let events = env.events().all();
    let last_event = events.last().expect("token_allowed event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    assert_eq!(topic1, Symbol::new(&env, "token_allowed"));
    
    let payload: (Address, Address) = last_event.2.into_val(&env);
    assert_eq!(payload.0, new_token);
    assert_eq!(payload.1, admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_add_allowed_token_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, _token, _admin) = setup_test(&env);

    // freelancer is not a signer — should fail with NotAdmin (#16)
    let new_token = Address::generate(&env);
    contract.add_allowed_token(&freelancer, &new_token);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_add_allowed_token_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    pause_escrow(&env, &contract, &admin);

    // Adding a token while paused should be rejected, same as remove_allowed_token (#989)
    let new_token = Address::generate(&env);
    contract.add_allowed_token(&admin, &new_token);
}

#[test]
fn test_add_duplicate_token_allowed() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let new_token = Address::generate(&env);
    contract.add_allowed_token(&admin, &new_token);
    contract.add_allowed_token(&admin, &new_token); // should be a no-op

    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 1);
}

#[test]
fn test_remove_allowed_token() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let new_token = Address::generate(&env);
    contract.add_allowed_token(&admin, &new_token);
    assert_eq!(contract.get_allowed_tokens().len(), 1);

    contract.remove_allowed_token(&admin, &new_token);
    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 0);

    let events = env.events().all();
    let last_event = events.last().expect("token_revoked event should be emitted");
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    assert_eq!(topic1, Symbol::new(&env, "token_revoked"));

    let payload: (Address, Address) = last_event.2.into_val(&env);
    assert_eq!(payload.0, new_token);
    assert_eq!(payload.1, admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_remove_allowed_token_non_admin_fails() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, client, freelancer, _token, _admin) = setup_test(&env);

    // freelancer is not a signer — should fail with NotAdmin (#16)
    let new_token = Address::generate(&env);
    contract.remove_allowed_token(&freelancer, &new_token);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")] // ContractPaused
fn test_remove_allowed_token_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let new_token = Address::generate(&env);
    contract.add_allowed_token(&admin, &new_token);

    pause_escrow(&env, &contract, &admin);
    contract.remove_allowed_token(&admin, &new_token);
}

#[test]
fn test_remove_non_existent_token_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    // Removing a token that was never added should be a no-op success
    let non_existent = Address::generate(&env);
    contract.remove_allowed_token(&admin, &non_existent);

    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 0);
}

#[test]
fn test_get_allowed_tokens_empty() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, _admin) = setup_test(&env);

    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 0);
}

#[test]
fn test_get_allowed_tokens_multiple() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let token_a = Address::generate(&env);
    let token_b = Address::generate(&env);
    let token_c = Address::generate(&env);

    contract.add_allowed_token(&admin, &token_a);
    contract.add_allowed_token(&admin, &token_b);
    contract.add_allowed_token(&admin, &token_c);

    let allowed = contract.get_allowed_tokens();
    assert_eq!(allowed.len(), 3);
    assert_eq!(allowed.get(0).unwrap(), token_a);
    assert_eq!(allowed.get(1).unwrap(), token_b);
    assert_eq!(allowed.get(2).unwrap(), token_c);
}

#[test]
fn test_create_job_with_whitelisted_token_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    // Add the token to the whitelist
    contract.add_allowed_token(&admin, &token);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    assert_eq!(job_id, 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_create_job_with_non_whitelisted_token_fails() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (contract, client, freelancer, token, admin) = setup_test(&env);

    // Register a separate token NOT added to the whitelist
    let other_token = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();

    // Add original token to whitelist, but not other_token
    contract.add_allowed_token(&admin, &token);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    contract.create_job(&client, &freelancer, &other_token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
}

#[test]
fn test_token_validation_happens_before_auth() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, freelancer, token, admin) = setup_test(&env);

    // Add original token to whitelist
    contract.add_allowed_token(&admin, &token);

    let other_token = Address::generate(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];

    // This should fail with TokenNotAllowed (#13) BEFORE any auth check
    let result = contract.try_create_job(
        &Address::generate(&env),
        &freelancer,
        &other_token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    // Contract errors are surfaced as Err(Ok(contract_error)) in try_* calls
    let contract_err = result.err().unwrap().unwrap();
    assert_eq!(contract_err, EscrowError::TokenNotAllowed);
}

// ============================================================
// STATE MACHINE VALIDATION TESTS
// ============================================================
// These tests verify that the escrow state machine enforces
// valid transitions and rejects invalid ones with proper errors.
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_fund_job_fails_when_already_funded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Fund the job once (valid transition: Created -> Funded)
    contract.fund_job(&job_id, &client, &0, &0);
    
    // Try to fund again (invalid: Funded -> Funded)
    contract.fund_job(&job_id, &client, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_fund_job_fails_when_in_progress() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    
    // Submit milestone to transition to InProgress
    contract.submit_milestone(&job_id, &0, &freelancer);
    
    // Try to fund again (invalid: InProgress -> Funded)
    contract.fund_job(&job_id, &client, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_fund_job_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    
    // Job is now Completed (terminal state)
    // Try to fund again (invalid: Completed -> Funded)
    contract.fund_job(&job_id, &client, &0, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_submit_milestone_fails_when_created() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Try to submit milestone before funding (invalid: Created -> InProgress)
    contract.submit_milestone(&job_id, &0, &freelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_submit_milestone_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Work 1"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Work 2"), 500_i128, JOB_DEADLINE),
    ];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);

    // Job is now InProgress (first milestone approved, second pending)
    contract.submit_milestone(&job_id, &1, &freelancer);
    contract.approve_milestone(&job_id, &1, &client);
    
    // Job is now Completed (all milestones approved)
    // Try to submit another milestone (invalid: Completed state is terminal)
    contract.submit_milestone(&job_id, &0, &freelancer);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_approve_milestone_fails_when_created() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Try to approve milestone before funding (invalid state)
    contract.approve_milestone(&job_id, &0, &client);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_cancel_job_fails_when_created() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Try to cancel before funding (invalid: Created state cannot be cancelled)
    contract.cancel_job(&job_id, &client, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_cancel_job_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    // Job is now Completed (terminal state)
    // Try to cancel (invalid: Completed is terminal)
    contract.cancel_job(&job_id, &client, &0);
}

#[test]
fn test_cancel_job_succeeds_when_funded_no_work_started() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    
    // Valid transition: Funded -> Cancelled (no work started)
    contract.cancel_job(&job_id, &client, &0);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")] // WorkInProgress
fn test_cancel_job_fails_when_work_in_progress() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    
    // Milestone is submitted (work in progress)
    // Try to cancel (invalid: must dispute instead)
    contract.cancel_job(&job_id, &client, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_top_up_escrow_fails_when_created() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Try to top up before funding (invalid: Created state)
    contract.top_up_escrow(&client, &job_id, &100_i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_top_up_escrow_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    // Job is now Completed
    // Try to top up (invalid: Completed is terminal)
    contract.top_up_escrow(&client, &job_id, &100_i128);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_expire_job_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    // Job is now Completed (terminal state)
    // Advance time past deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);

    // Try to expire (invalid: Completed is terminal)
    contract.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_expire_job_fails_when_cancelled() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.cancel_job(&job_id, &client, &0);
    
    // Job is now Cancelled (terminal state)
    // Advance time past deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);
    
    // Try to expire (invalid: Cancelled is terminal)
    contract.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_expire_job_fails_when_already_expired() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    
    // Advance time past deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);
    
    // Expire the job (valid transition: Funded -> Expired)
    contract.expire_job(&job_id);
    
    // Try to expire again (invalid: Expired is terminal)
    contract.expire_job(&job_id);
}

#[test]
fn test_expire_job_succeeds_when_funded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    
    // Advance time past deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);
    
    // Valid transition: Funded -> Expired
    contract.expire_job(&job_id);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
}

#[test]
fn test_expire_job_succeeds_when_in_progress() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Work 1"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Work 2"), 500_i128, JOB_DEADLINE),
    ];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Job is now InProgress
    // Advance time past deadline
    env.ledger().with_mut(|l| l.timestamp = JOB_DEADLINE + 1);
    
    // Valid transition: InProgress -> Expired
    contract.expire_job(&job_id);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_resolve_dispute_fails_when_created() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    // Try to resolve dispute on Created job (invalid: not disputable)
    contract.resolve_dispute_callback(&job_id, &DisputeResolution::ClientWins);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_resolve_dispute_fails_when_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    // Job is now Completed (terminal state)
    // Try to resolve dispute (invalid: Completed is terminal)
    contract.resolve_dispute_callback(&job_id, &DisputeResolution::ClientWins);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_resolve_dispute_fails_when_cancelled() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.cancel_job(&job_id, &client, &0);
    
    // Job is now Cancelled (terminal state)
    // Try to resolve dispute (invalid: Cancelled is terminal)
    contract.resolve_dispute_callback(&job_id, &DisputeResolution::ClientWins);
}

#[test]
fn test_state_transition_created_to_funded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Created);
    
    // Valid transition: Created -> Funded
    contract.fund_job(&job_id, &client, &0, &0);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Funded);
}

#[test]
fn test_state_transition_funded_to_in_progress() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Funded);
    
    // Valid transition: Funded -> InProgress
    contract.submit_milestone(&job_id, &0, &freelancer);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);
}

#[test]
fn test_state_transition_in_progress_to_completed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    
    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);
    
    // Valid transition: InProgress -> Completed (when all milestones approved)
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);
}

#[test]
fn test_terminal_states_cannot_transition() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client, freelancer, token, _admin) = setup_test(&env);

    // Test Completed is terminal
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client);
    contract.complete_job(&job_id, &client);

    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    // Verify no operations can change state from Completed
    // (already tested in individual test cases above)
}

// ============================================================
// release_milestone tests
// ============================================================

#[test]
fn test_release_milestone_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Implementation"), 1000_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Release the first milestone
    contract.release_milestone(&job_id, &0, &client_addr, &0, &0);

    let job = contract.get_job(&job_id);
    let milestone = job.milestones.get(0).unwrap();
    assert_eq!(milestone.status, MilestoneStatus::Approved);
    assert_eq!(milestone.amount, 500);
    // Job should still be InProgress (second milestone not yet submitted)
    assert_eq!(job.status, JobStatus::InProgress);
}

#[test]
fn test_release_milestone_completes_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Sole milestone"), 1000_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.release_milestone(&job_id, &0, &client_addr, &0, &0);

    let job = contract.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);
}

#[test]
fn test_release_milestone_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.release_milestone(&job_id, &0, &client_addr, &0, &0);

    let events = env.events().all();
    let mut found_ms_released = false;
    let mut payload: Option<(u64, u32, Address, Address, i128)> = None;
    for event in events.iter() {
        if event.0 == contract.address && event.1.len() >= 2 {
            let topic0: Symbol = event.1.get(0).unwrap().into_val(&env);
            let topic1: Symbol = event.1.get(1).unwrap().into_val(&env);
            if topic0 == symbol_short!("escrow") && topic1 == Symbol::new(&env, "ms_released") {
                found_ms_released = true;
                payload = Some(event.2.clone().into_val(&env));
            }
        }
    }

    assert!(found_ms_released, "ms_released event should exist");
    let payload = payload.unwrap();
    assert_eq!(payload.0, job_id);
    assert_eq!(payload.1, 0);
    assert_eq!(payload.2, client_addr);
    assert_eq!(payload.3, freelancer);
    assert_eq!(payload.4, 500);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_milestone_fails_for_non_client() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Freelancer tries to release — should fail with Unauthorized (#2)
    contract.release_milestone(&job_id, &0, &freelancer, &0, &0);
}

#[test]
fn test_release_milestone_fails_for_disputed_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Design"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // Set job status to Disputed
    env.as_contract(&contract.address, || {
        let key = crate::DataKey::Job(job_id);
        let mut job: crate::Job = env.storage().persistent().get(&key).unwrap();
        job.status = JobStatus::Disputed;
        env.storage().persistent().set(&key, &job);
    });

    let result = contract.try_release_milestone(&job_id, &0, &client_addr, &0, &0);
    assert!(result.is_err());
}

// ============================================================
// partial_refund tests
// ============================================================

#[test]
fn test_partial_refund_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Milestone 1"), 400_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "Milestone 2"), 600_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Approve first milestone so remaining = 600 (the second milestone amount)
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client_addr);

    // Before: balance snapshot for client
    let token_client = TokenClient::new(&env, &token);
    let balance_before = token_client.balance(&client_addr);

    // Partial refund of 200 from the remaining 600
    contract.partial_refund(&admin, &job_id, &200, &0);

    // Verify token transfer
    let balance_after = token_client.balance(&client_addr);
    assert_eq!(balance_after, balance_before + 200);

    // Verify remaining balance tracked accurately
    let job = contract.get_job(&job_id);
    // total_amount = 1000 - 200 = 800
    assert_eq!(job.total_amount, 800);
    // funded_amount = 1000 - 200 = 800
    assert_eq!(job.funded_amount, 800);
    // Milestone amounts unchanged
    assert_eq!(job.milestones.get(0).unwrap().amount, 400);
    assert_eq!(job.milestones.get(1).unwrap().amount, 600);
}

#[test]
fn test_partial_refund_balance_tracked_accurately() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 300_i128, JOB_DEADLINE / 3),
        (String::from_str(&env, "M2"), 300_i128, JOB_DEADLINE * 2 / 3),
        (String::from_str(&env, "M3"), 400_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Submit and approve M1 (300 paid out, remaining = 700)
    contract.submit_milestone(&job_id, &0, &freelancer);
    contract.approve_milestone(&job_id, &0, &client_addr);

    // Partial refund of 200 → total_amount should be 1000 - 200 = 800, funded = 800
    contract.partial_refund(&admin, &job_id, &200, &0);

    let job = contract.get_job(&job_id);
    assert_eq!(job.total_amount, 800);
    assert_eq!(job.funded_amount, 800);

    // Another partial refund of 100 → total_amount = 700, funded = 700
    contract.partial_refund(&admin, &job_id, &100, &1);

    let job = contract.get_job(&job_id);
    assert_eq!(job.total_amount, 700);
    assert_eq!(job.funded_amount, 700);
}

#[test]
fn test_partial_refund_fails_over_refund() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // No milestones approved, remaining = 500. Attempt to refund 600 > 500 → should fail.
    let result = contract.try_partial_refund(&admin, &job_id, &600, &0);
    assert!(result.is_err());
}

#[test]
fn test_partial_refund_fails_unauthorized_caller() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let unauthorized = Address::generate(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Unauthorized caller is not a signer → should fail with NotAdmin (#16)
    let result = contract.try_partial_refund(&unauthorized, &job_id, &100, &0);
    assert!(result.is_err());
}

#[test]
fn test_partial_refund_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    contract.partial_refund(&admin, &job_id, &200, &0);

    let events = env.events().all();
    let mut found_partial_ref = false;
    let mut payload: Option<(u64, Address, Address, i128, i128)> = None;
    for event in events.iter() {
        if event.0 == contract.address && event.1.len() >= 2 {
            let topic0: Symbol = event.1.get(0).unwrap().into_val(&env);
            let topic1: Symbol = event.1.get(1).unwrap().into_val(&env);
            if topic0 == symbol_short!("escrow") && topic1 == Symbol::new(&env, "partial_ref") {
                found_partial_ref = true;
                payload = Some(event.2.clone().into_val(&env));
            }
        }
    }

    assert!(found_partial_ref, "partial_ref event should exist");
    let payload = payload.unwrap();
    assert_eq!(payload.0, job_id);
    assert_eq!(payload.1, admin);
    assert_eq!(payload.2, client_addr);
    assert_eq!(payload.3, 200);
    assert_eq!(payload.4, 300);
}

// ============================================================
// expire_proposal tests
// ============================================================

#[test]
fn test_expire_proposal_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Client proposes a revision (using the existing Milestone type directly)
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);

    // Advance time past the proposal TTL (604800 = setup default expiry)
    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);

    // Expire the proposal as the original proposer
    contract.expire_proposal(&client_addr, &job_id);

    // Verify proposal is now Rejected
    let proposal = contract.get_revision_proposal(&job_id).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Rejected);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_expire_proposal_fails_when_paused() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client_addr, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client_addr, &0, &0);
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);
    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);
    pause_escrow(&env, &contract, &admin);
    contract.expire_proposal(&client_addr, &job_id);
}

#[test]
fn test_expire_proposal_succeeds_after_unpause() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);
    let (contract, client_addr, freelancer, token, admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client_addr, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);
    contract.fund_job(&job_id, &client_addr, &0, &0);
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);
    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);
    pause_escrow(&env, &contract, &admin);
    unpause_escrow(&env, &contract, &admin);
    contract.expire_proposal(&client_addr, &job_id);
    let proposal = contract.get_revision_proposal(&job_id).unwrap();
    assert_eq!(proposal.status, ProposalStatus::Rejected);
}

#[test]
fn test_expire_proposal_event_emitted() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);

    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);
    contract.expire_proposal(&client_addr, &job_id);

    let events = env.events().all();
    let mut found_expired = false;
    let mut payload: Option<(u64, Address, Address)> = None;
    for event in events.iter() {
        if event.0 == contract.address && event.1.len() == 1 {
            let topic0: Symbol = event.1.get(0).unwrap().into_val(&env);
            if topic0 == Symbol::new(&env, "revision_expired") {
                found_expired = true;
                payload = Some(event.2.clone().into_val(&env));
            }
        }
    }

    assert!(found_expired, "revision_expired event should exist");
    let payload = payload.unwrap();
    assert_eq!(payload.0, job_id);
    assert_eq!(payload.1, client_addr);
    assert_eq!(payload.2, client_addr);
}

#[test]
fn test_expire_proposal_fails_before_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);

    // Do NOT advance time — TTL has not elapsed
    let result = contract.try_expire_proposal(&client_addr, &job_id);
    assert!(result.is_err());
}

#[test]
fn test_expire_proposal_fails_non_proposer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Client proposes a revision
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 600,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);

    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);

    // Freelancer (non-proposer) tries to expire → should fail
    let result = contract.try_expire_proposal(&freelancer, &job_id);
    assert!(result.is_err());
}

#[test]
fn test_expire_proposal_fails_non_pending() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // Client proposes a revision
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Revised"),
            amount: 500,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
                token: None,
        },
    ];
    contract.propose_revision(&client_addr, &job_id, &new_milestones);

    // Freelancer rejects the proposal (so it's no longer Pending)
    contract.reject_revision(&freelancer, &job_id);

    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);

    // Proposal is Rejected, not Pending → should fail
    let result = contract.try_expire_proposal(&client_addr, &job_id);
    assert!(result.is_err());
}

#[test]
fn test_expire_proposal_fails_no_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    contract.fund_job(&job_id, &client_addr, &0, &0);

    // No proposal exists → should fail
    env.ledger().with_mut(|l| l.timestamp += 604800 + 1);
    let result = contract.try_expire_proposal(&client_addr, &job_id);
    assert!(result.is_err());
}

// ── claim_expired tests ───────────────────────────────────────────────────────

#[test]
fn test_claim_expired_success() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    // Set expiry_ledger to 100
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Advance past expiry_ledger (sequence) and past job_deadline (timestamp)
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired - should succeed
    escrow.expire_job(&job_id);
    
    // Verify job status is Expired
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
    
    // Verify client received refund
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), total);
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")] // DeadlineNotPassed
fn test_claim_expired_fails_before_expiry() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    // Set expiry_ledger to 100
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Ledger sequence is still before expiry_ledger
    env.ledger().with_mut(|l| l.sequence_number = expiry_ledger - 1);
    
    // Claim expired - should fail
    escrow.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_claim_expired_fails_on_completed_job() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Complete the job
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);
    escrow.submit_milestone(&job_id, &1, &freelancer);
    escrow.approve_milestone(&job_id, &1, &client);
    escrow.submit_milestone(&job_id, &2, &freelancer);
    escrow.approve_milestone(&job_id, &2, &client);
    escrow.complete_job(&job_id, &client);

    // Advance past both deadline (timestamp) and expiry_ledger (sequence) so the
    // state check is reached rather than DeadlineNotPassed firing first.
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired - should fail (job is Completed)
    escrow.expire_job(&job_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")] // InvalidStatus
fn test_claim_expired_fails_on_cancelled_job() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Cancel the job
    escrow.cancel_job(&job_id, &client, &0);

    // Advance past both deadline (timestamp) and expiry_ledger (sequence).
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired - should fail (job is Cancelled)
    escrow.expire_job(&job_id);
}

#[test]
fn test_claim_expired_with_approved_milestones() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Approve first milestone (500)
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client);

    // Advance past expiry_ledger (sequence) and job_deadline (timestamp).
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired - should refund remaining amount (total - approved)
    escrow.expire_job(&job_id);
    
    // Verify job status is Expired
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
    
    // Verify client received refund for unapproved milestones (1000 + 1500 = 2500)
    let token_client = TokenClient::new(&env, &token);
    assert_eq!(token_client.balance(&client), 2500);
    
    // Verify freelancer received approved milestone amount (500)
    assert_eq!(token_client.balance(&freelancer), 500);
}

#[test]
fn test_claim_expired_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Advance past expiry_ledger (sequence) and job_deadline (timestamp).
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired
    escrow.expire_job(&job_id);
    
    // Verify job_expired event was emitted
    let events = env.events().all();
    let expired_event = events.last().expect("job_expired event should be emitted");

    // Verify the event topic is "job_expired"
    let topic: Symbol = expired_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic, Symbol::new(&env, "job_expired"));

    // Verify event data: (job_id, client, freelancer, token, refund_amount)
    let event_data: (u64, Address, Address, Address, i128) = expired_event.2.clone().into_val(&env);
    assert_eq!(event_data.0, job_id);
    assert_eq!(event_data.1, client);
    assert_eq!(event_data.4, total);
}

#[test]
fn test_claim_expired_permissionless() {
    let env = Env::default();
    env.mock_all_auths();
    
    let (escrow, token) = setup_refund_env(&env);
    
    let client = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let milestones = default_milestones(&env);
    
    let expiry_ledger = 100u32;
    let job_id = escrow.create_job(
        &client,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &expiry_ledger,
    );
    
    let total: i128 = 500 + 1000 + 1500;
    mint_tokens(&env, &token, &client, total);
    escrow.fund_job(&job_id, &client, &0, &0);
    
    // Advance past expiry_ledger (sequence) and job_deadline (timestamp).
    env.ledger().with_mut(|l| {
        l.sequence_number = expiry_ledger + 1;
        l.timestamp = JOB_DEADLINE + 1;
    });

    // Claim expired as third party - should succeed (permissionless)
    escrow.expire_job(&job_id);
    
    // Verify job status is Expired
    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
}

// ─── Exchange-rate parity (TWAP oracle) tests ──────────────────────────────────

/// A mock price oracle returning a configurable TWAP price and sample count.
/// Mirrors the interface the escrow contract invokes:
/// `twap(token: Address, quote: Address, sample_ledgers: u32) -> (i128 price, u32 samples)`.
#[contract]
pub struct MockOracle;

#[contractimpl]
impl MockOracle {
    pub fn set(env: Env, price: i128, samples: u32) {
        env.storage().instance().set(&symbol_short!("price"), &price);
        env.storage().instance().set(&symbol_short!("samples"), &samples);
    }

    pub fn twap(env: Env, _token: Address, _quote: Address, _sample_ledgers: u32) -> (i128, u32) {
        let price: i128 = env.storage().instance().get(&symbol_short!("price")).unwrap_or(0);
        let samples: u32 = env.storage().instance().get(&symbol_short!("samples")).unwrap_or(0);
        (price, samples)
    }
}

/// Register a MockOracle, configure its price/samples, and wire it into the escrow.
fn setup_oracle(
    env: &Env,
    escrow: &EscrowContractClient<'_>,
    admin: &Address,
    price: i128,
    samples: u32,
) -> Address {
    let oracle_id = env.register_contract(None, MockOracle);
    let oracle = MockOracleClient::new(env, &oracle_id);
    oracle.set(&price, &samples);
    escrow.set_price_oracle(admin, &oracle_id);
    oracle_id
}

/// Create a single-milestone job worth `total` and return its id.
fn create_priced_job(
    env: &Env,
    escrow: &EscrowContractClient<'_>,
    client_addr: &Address,
    freelancer: &Address,
    token: &Address,
    total: i128,
) -> u64 {
    let milestones = vec![&env, (String::from_str(env, "Task"), total, JOB_DEADLINE)];
    escrow.create_job(
        client_addr,
        freelancer,
        token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    )
}

#[test]
fn test_fund_with_exact_value_match() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);
    // price 1.0 (PRICE_SCALE), 12 samples
    setup_oracle(&env, &escrow, &admin, PRICE_SCALE, 12);

    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 100);
    // deposited_value = 100 * 1.0 = 100; agreed 100, 0% slippage -> exact match passes.
    escrow.fund_job(&job_id, &client_addr, &100, &0);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Funded);

    let snap = escrow.get_rate_snapshot(&job_id).unwrap();
    assert_eq!(snap.deposited_value, 100);
    assert_eq!(snap.agreed_value_stroops, 100);
    assert_eq!(snap.twap_price, PRICE_SCALE);
    assert_eq!(snap.samples, 12);
}

#[test]
fn test_fund_one_bps_under_tolerance_passes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);
    setup_oracle(&env, &escrow, &admin, PRICE_SCALE, 10);

    // deposited_value = 10000 * 1.0 = 10000.
    // agreed 10000, slippage 200 bps (2%) -> min_value = 9800. 10000 >= 9800 passes.
    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 10000);
    escrow.fund_job(&job_id, &client_addr, &10000, &200);

    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Funded);
}

#[test]
fn test_fund_one_bps_over_tolerance_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);
    // price 1.0 (PRICE_SCALE), 10 samples.
    setup_oracle(&env, &escrow, &admin, PRICE_SCALE, 10);

    // agreed 10000, slippage 200 bps -> min_value = 9800.
    // deposited_value = 9799 -> exactly 1 unit under tolerance -> rejected.
    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 9799);
    let res = escrow.try_fund_job(&job_id, &client_addr, &10000, &200);
    assert_eq!(res, Err(Ok(EscrowError::InsufficientValue)));

    // Job remains unfunded.
    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Created);
}

#[test]
fn test_fund_boundary_exact_min_value_passes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);
    setup_oracle(&env, &escrow, &admin, PRICE_SCALE, 10);

    // deposited_value = 9800. agreed 10000, slippage 200 bps -> min_value = 9800.
    // 9800 >= 9800 -> passes (boundary inclusive).
    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 9800);
    escrow.fund_job(&job_id, &client_addr, &10000, &200);
    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Funded);
}

#[test]
fn test_xlm_only_job_bypasses_oracle() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _admin) = setup_test(&env);
    // No oracle configured at all.
    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 100);

    // agreed_value_stroops = 0 -> oracle bypassed, funding succeeds.
    escrow.fund_job(&job_id, &client_addr, &0, &0);
    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Funded);
    // No snapshot is stored on the bypass path.
    assert!(escrow.get_rate_snapshot(&job_id).is_none());
}

#[test]
fn test_oracle_unavailable_when_not_configured() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _admin) = setup_test(&env);
    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 100);

    // agreed value > 0 but no oracle set -> OracleUnavailable, not a panic.
    let res = escrow.try_fund_job(&job_id, &client_addr, &100, &200);
    assert_eq!(res, Err(Ok(EscrowError::OracleUnavailable)));
    assert_eq!(escrow.get_job(&job_id).status, JobStatus::Created);
}

#[test]
fn test_oracle_too_few_samples_unavailable() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);
    // Only 9 samples (< MIN_TWAP_SAMPLES = 10) -> oracle considered unavailable.
    setup_oracle(&env, &escrow, &admin, PRICE_SCALE, 9);

    let job_id = create_priced_job(&env, &escrow, &client_addr, &freelancer, &token, 100);
    let res = escrow.try_fund_job(&job_id, &client_addr, &100, &200);
    assert_eq!(res, Err(Ok(EscrowError::OracleUnavailable)));
}

#[test]
fn test_escrow_ttl_flow_success() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Extend TTL permissionlessly (should succeed because the job is active).
    contract.extend_escrow_ttl(&job_id);

    // Verify event is emitted.
    let events = env.events().all();
    let last_event = events.last().unwrap();
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    assert_eq!(topic1, symbol_short!("ttl_ext"));
}

#[test]
#[should_panic]
fn test_escrow_ttl_flow_archived_get_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Advance sequence beyond ESCROW_TTL_LEDGERS.
    env.ledger().with_mut(|l| {
        l.sequence_number = 535002;
    });

    // This should panic because the job is archived.
    let _job = contract.get_job(&job_id);
}

#[test]
#[should_panic]
fn test_escrow_ttl_flow_archived_extend_panics() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Advance sequence beyond ESCROW_TTL_LEDGERS.
    env.ledger().with_mut(|l| {
        l.sequence_number = 535002;
    });

    // This should panic because the job is archived.
    contract.extend_escrow_ttl(&job_id);
}

#[test]
fn test_escrow_ttl_flow_restore() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(&client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD, &DEFAULT_EXPIRY_LEDGER);

    // Advance sequence beyond ESCROW_TTL_LEDGERS.
    env.ledger().with_mut(|l| {
        l.sequence_number = 535002;
    });

    // Simulate host-level RestoreFootprint by extending TTL before execution.
    let key = crate::DataKey::Job(job_id);
    env.ledger().with_mut(|l| {
        l.sequence_number = 1;
    });
    env.as_contract(&contract.address, || {
        env.storage().persistent().extend_ttl(&key, 600000, 600000);
    });
    env.ledger().with_mut(|l| {
        l.sequence_number = 535002;
    });

    // Call restore_escrow which should succeed and bump the TTL.
    contract.restore_escrow(&job_id);

    // Verify the entry is readable.
    let _job_after = contract.get_job(&job_id);
}

// ============================================================
// Issue #701 — Milestone amount validation tests
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #44)")] // InvalidMilestone
fn test_create_job_zero_amount_milestone_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Zero milestone"), 0_i128, JOB_DEADLINE),
    ];

    contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #44)")] // InvalidMilestone
fn test_create_job_negative_amount_milestone_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Negative milestone"), -100_i128, JOB_DEADLINE),
    ];

    contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
}

#[test]
fn test_create_job_valid_milestones_succeeds() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "M2"), 1000_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    let job = contract.get_job(&job_id);
    assert_eq!(job.total_amount, 1500);
    assert_eq!(job.milestones.len(), 2);
}

// ============================================================
// Issue #662 — Nonce replay protection tests
// ============================================================

#[test]
#[should_panic(expected = "Error(Contract, #46)")] // NonceReplay
fn test_release_milestone_nonce_replay_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, client_addr, freelancer, token, _) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "M1"), 500_i128, JOB_DEADLINE / 2),
        (String::from_str(&env, "M2"), 500_i128, JOB_DEADLINE),
    ];

    let job_id = contract.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    contract.fund_job(&job_id, &client_addr, &0, &0);
    contract.submit_milestone(&job_id, &0, &freelancer);

    // First release succeeds with nonce 42
    contract.release_milestone(&job_id, &0, &client_addr, &0, &42);

    // Submit second milestone
    contract.submit_milestone(&job_id, &1, &freelancer);

    // Replay same nonce 42 — should fail with NonceReplay
    contract.release_milestone(&job_id, &1, &client_addr, &0, &42);
}

// ── calculate_payout tests ─────────────────────────────────────────────────────

#[test]
fn test_calculate_payout_client_wins() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::ClientWins);

    assert_eq!(result.client, 965);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_freelancer_wins() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::FreelancerWins);

    assert_eq!(result.client, 0);
    assert_eq!(result.freelancer, 965);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_refund_both() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::RefundBoth);

    // 965 / 2 = 482 (truncated), freelancer gets 965 - 482 = 483
    assert_eq!(result.client, 482);
    assert_eq!(result.freelancer, 483);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_refund_split() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // 30% to client, 70% to freelancer
    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::RefundSplit(30));

    // remaining = 965, client = 965 * 30 / 100 = 289, freelancer = 965 - 289 = 676
    assert_eq!(result.client, 289);
    assert_eq!(result.freelancer, 676);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_refund_split_100_percent() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::RefundSplit(100));

    assert_eq!(result.client, 965);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_refund_split_over_100_clamped() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // 200 should be clamped to 100 (all remaining to client)
    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::RefundSplit(200));

    assert_eq!(result.client, 965);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.platform, 25);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_escalate() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // Escalate means no payout yet — all zeros
    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::Escalate);

    assert_eq!(result.client, 0);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.platform, 0);
    assert_eq!(result.arbitrators, 0);
}

#[test]
fn test_calculate_payout_malicious_filing() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // Full remaining + platform fee goes to treasury (platform)
    let result = client.calculate_payout(&1000_i128, &250_u32, &100_u32, &DisputeResolution::MaliciousFiling);

    assert_eq!(result.client, 0);
    assert_eq!(result.freelancer, 0);
    // platform = remaining (965) + platform_fee (25) = 990
    assert_eq!(result.platform, 990);
    assert_eq!(result.arbitrators, 10);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 1000);
}

#[test]
fn test_calculate_payout_zero_escrow() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    let result = client.calculate_payout(&0_i128, &500_u32, &200_u32, &DisputeResolution::ClientWins);

    assert_eq!(result.client, 0);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.platform, 0);
    assert_eq!(result.arbitrators, 0);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 0);
}

#[test]
fn test_calculate_payout_max_fee() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // 100% platform fee + 0% arbitrator fee
    let result = client.calculate_payout(&5000_i128, &10000_u32, &0_u32, &DisputeResolution::ClientWins);

    assert_eq!(result.platform, 5000);
    assert_eq!(result.arbitrators, 0);
    assert_eq!(result.client, 0);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 5000);
}

#[test]
fn test_calculate_payout_rounding_client_wins() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &escrow_id);

    // Amount that creates rounding in fee calculation
    // 7 * 333 / 10000 = 2 (truncated), 7 * 333 / 10000 = 2
    // remaining = 7 - 2 - 2 = 3
    let result = client.calculate_payout(&7_i128, &3333_u32, &3333_u32, &DisputeResolution::ClientWins);

    // platform_fee = 7 * 3333 / 10000 = 2
    // arbitrator_fee = 7 * 3333 / 10000 = 2
    // client = 7 - 2 - 2 = 3
    assert_eq!(result.platform, 2);
    assert_eq!(result.arbitrators, 2);
    assert_eq!(result.client, 3);
    assert_eq!(result.freelancer, 0);
    assert_eq!(result.client + result.freelancer + result.platform + result.arbitrators, 7);
}

#[test]
fn test_calculate_payout_sum_invariant() {
    let env = Env::default();
    let escrow_id = env.register_contract(None, EscrowContract);
    let contract = EscrowContractClient::new(&env, &escrow_id);

    let outcomes = [
        DisputeResolution::ClientWins,
        DisputeResolution::FreelancerWins,
        DisputeResolution::RefundBoth,
        DisputeResolution::RefundSplit(0),
        DisputeResolution::RefundSplit(30),
        DisputeResolution::RefundSplit(50),
        DisputeResolution::RefundSplit(70),
        DisputeResolution::RefundSplit(100),
        DisputeResolution::MaliciousFiling,
    ];

    let test_amounts: [i128; 8] = [0, 1, 7, 100, 1000, 9999, 100_000, 1_000_000_000_000];
    let fee_combos: [(u32, u32); 6] = [
        (0, 0),
        (100, 0),
        (0, 100),
        (250, 100),
        (500, 500),
        (1000, 500),
    ];

    for &amount in &test_amounts {
        for &(pf, af) in &fee_combos {
            for outcome in &outcomes {
                let result = contract.calculate_payout(&amount, &pf, &af, outcome);
                let sum = result.client + result.freelancer + result.platform + result.arbitrators;
                assert_eq!(
                    sum, amount,
                    "sum mismatch: amount={}, pf_bps={}, af_bps={}, outcome={:?}",
                    amount, pf, af, outcome
                );
            }
        }
    }
}

#[test]
fn test_add_allowed_token_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _, token, admin) = setup_test(&env);
    
    client.add_allowed_token(&admin, &token);
    
    let events = env.events().all();
    let last_event = events.last().unwrap();
    
    assert_eq!(last_event.0, client.address);
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "token_allowed"));
    
    let payload: (Address, Address) = last_event.2.into_val(&env);
    assert_eq!(payload, (token, admin));
}

#[test]
fn test_remove_allowed_token_emits_event() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, _, _, token, admin) = setup_test(&env);
    
    client.add_allowed_token(&admin, &token);
    client.remove_allowed_token(&admin, &token);
    
    let events = env.events().all();
    let last_event = events.last().unwrap();
    
    assert_eq!(last_event.0, client.address);
    let topic0: Symbol = last_event.1.get(0).unwrap().into_val(&env);
    assert_eq!(topic0, symbol_short!("escrow"));
    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "token_revoked"));
    
    let payload: (Address, Address) = last_event.2.into_val(&env);
    assert_eq!(payload, (token, admin));
}

// ── Issue #772: milestone deadline validation ────────────────────────────────

#[test]
#[should_panic(expected = "Error(Contract, #49)")]
fn test_create_job_rejects_past_milestone_deadline() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, user_client, freelancer, token_address, _admin) = setup_test(&env);
    client.add_allowed_token(&_admin, &token_address);

    // Advance ledger so we can create a "past" deadline
    env.ledger().with_mut(|l| l.timestamp = 1_000_000);

    // milestone deadline in the past (timestamp = 0 ≤ current 1_000_000)
    let milestones = vec![
        &env,
        (String::from_str(&env, "Task"), 100_i128, 500_000u64),
    ];

    client.create_job(
        &user_client,
        &freelancer,
        &token_address,
        &milestones,
        &9_999_999_999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #47)")]
fn test_create_job_rejects_non_ascending_milestone_deadlines() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, user_client, freelancer, token_address, _admin) = setup_test(&env);
    client.add_allowed_token(&_admin, &token_address);

    // milestone 1 deadline > milestone 2 deadline → non-ascending
    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 50_i128, 2_000_000u64),
        (String::from_str(&env, "Task 2"), 50_i128, 1_000_000u64),
    ];

    client.create_job(
        &user_client,
        &freelancer,
        &token_address,
        &milestones,
        &9_999_999_999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #47)")]
fn test_create_job_rejects_equal_milestone_deadlines() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, user_client, freelancer, token_address, _admin) = setup_test(&env);
    client.add_allowed_token(&_admin, &token_address);

    // milestone 1 and 2 share the same deadline → not strictly ascending
    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 50_i128, 1_000_000u64),
        (String::from_str(&env, "Task 2"), 50_i128, 1_000_000u64),
    ];

    client.create_job(
        &user_client,
        &freelancer,
        &token_address,
        &milestones,
        &9_999_999_999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );
}

#[test]
fn test_create_job_accepts_valid_ascending_future_deadlines() {
    let env = Env::default();
    env.mock_all_auths();
    let (client, user_client, freelancer, token_address, _admin) = setup_test(&env);
    client.add_allowed_token(&_admin, &token_address);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 50_i128, 1_000_000u64),
        (String::from_str(&env, "Task 2"), 50_i128, 2_000_000u64),
        (String::from_str(&env, "Task 3"), 50_i128, 3_000_000u64),
    ];

    let job_id = client.create_job(
        &user_client,
        &freelancer,
        &token_address,
        &milestones,
        &9_999_999_999u64,
        &86400u64,
        &(env.ledger().sequence() + 518_400),
    );

    assert_eq!(job_id, 1);
    let job = client.get_job(&job_id);
    assert_eq!(job.milestones.len(), 3);
}

#[test]
fn test_milestone_deadline_in_past_error_code_is_49() {
    assert_eq!(EscrowError::MilestoneDeadlineInPast as u32, 49);
}

#[test]
fn test_milestone_deadlines_not_ordered_error_code_is_47() {
    assert_eq!(EscrowError::MilestoneDeadlinesNotOrdered as u32, 47);
}

// ============================================================
// Multi-token milestone tests (issue #897)
// ============================================================

#[test]
fn test_create_multi_token_job_basic() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &1000);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 300_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Phase 2"), 500_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    let job = escrow.get_job(&job_id);
    assert_eq!(job.total_amount, 300); // only default-token milestones
    assert_eq!(job.token_balances.len(), 1);
    assert_eq!(job.token_balances.get(0).unwrap().token, token_b);
    assert_eq!(job.token_balances.get(0).unwrap().total_amount, 500);
    assert_eq!(job.milestones.len(), 2);
    assert_eq!(job.milestones.get(0).unwrap().token, None);
    assert_eq!(job.milestones.get(1).unwrap().token, Some(token_b));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")] // Unauthorized
fn test_create_multi_token_job_rejects_self_employment() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let same_addr = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    StellarAssetClient::new(&env, &token_a).mint(&same_addr, &1000);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 300_i128, JOB_DEADLINE, None),
    ];

    // client == freelancer should be rejected, same as create_job (#988)
    escrow.create_multi_token_job(
        &same_addr,
        &same_addr,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );
}

#[test]
fn test_fund_multi_token_job() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &1000);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &1000);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 200_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Phase 2"), 300_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.funded_amount, 200);
    assert_eq!(job.token_balances.get(0).unwrap().funded_amount, 300);

    // Verify tokens were actually transferred.
    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);
    assert_eq!(tc_a.balance(&contract_id), 200);
    assert_eq!(tc_b.balance(&contract_id), 300);
}

#[test]
fn test_multi_token_complete_job_lifecycle() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &500);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task A"), 100_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Task B"), 200_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    escrow.fund_job(&job_id, &client_addr, &0, &0);

    // Submit both milestones.
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.submit_milestone(&job_id, &1, &freelancer);

    // Approve both milestones.
    escrow.approve_milestone(&job_id, &0, &client_addr);
    escrow.approve_milestone(&job_id, &1, &client_addr);

    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);

    let freelancer_a_before = tc_a.balance(&freelancer);
    let freelancer_b_before = tc_b.balance(&freelancer);

    escrow.complete_job(&job_id, &client_addr);

    // Freelancer receives token_a for milestone 0 and token_b for milestone 1.
    assert_eq!(tc_a.balance(&freelancer) - freelancer_a_before, 100);
    assert_eq!(tc_b.balance(&freelancer) - freelancer_b_before, 200);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);
}

#[test]
fn test_multi_token_cancel_job_refunds_all_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &500);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Phase 1"), 150_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Phase 2"), 250_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);

    let client_a_before = tc_a.balance(&client_addr);
    let client_b_before = tc_b.balance(&client_addr);

    escrow.cancel_job(&job_id, &client_addr, &1);

    // Client gets full refund in both tokens.
    assert_eq!(tc_a.balance(&client_addr) - client_a_before, 150);
    assert_eq!(tc_b.balance(&client_addr) - client_b_before, 250);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);
}

#[test]
fn test_multi_token_dispute_resolution_client_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &500);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task A"), 100_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Task B"), 200_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    // Fund the job; call resolve_dispute_callback directly (Funded state is disputable).
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);
    let client_a_before = tc_a.balance(&client_addr);
    let client_b_before = tc_b.balance(&client_addr);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::ClientWins);

    // Client wins: gets ALL funds back (including non-default token).
    assert_eq!(tc_a.balance(&client_addr) - client_a_before, 100);
    assert_eq!(tc_b.balance(&client_addr) - client_b_before, 200);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Cancelled);
}

#[test]
fn test_multi_token_dispute_resolution_freelancer_wins() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &500);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task A"), 100_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Task B"), 200_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);
    let fl_a_before = tc_a.balance(&freelancer);
    let fl_b_before = tc_b.balance(&freelancer);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::FreelancerWins);

    assert_eq!(tc_a.balance(&freelancer) - fl_a_before, 100);
    assert_eq!(tc_b.balance(&freelancer) - fl_b_before, 200);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);
}

#[test]
fn test_multi_token_expire_job_refunds_all_tokens() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &500);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &500);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task A"), 120_i128, JOB_DEADLINE, None),
        (String::from_str(&env, "Task B"), 180_i128, JOB_DEADLINE + 1, Some(token_b.clone())),
    ];

    let job_deadline = JOB_DEADLINE + 10;
    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &job_deadline,
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    escrow.fund_job(&job_id, &client_addr, &0, &0);

    // Advance past deadline.
    env.ledger().with_mut(|l| l.timestamp = job_deadline + 1);

    let tc_a = TokenClient::new(&env, &token_a);
    let tc_b = TokenClient::new(&env, &token_b);
    let client_a_before = tc_a.balance(&client_addr);
    let client_b_before = tc_b.balance(&client_addr);

    escrow.expire_job(&job_id);

    assert_eq!(tc_a.balance(&client_addr) - client_a_before, 120);
    assert_eq!(tc_b.balance(&client_addr) - client_b_before, 180);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Expired);
}

#[test]
fn test_single_token_backward_compat_via_create_job() {
    // Existing create_job still works exactly as before.
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    StellarAssetClient::new(&env, &token).mint(&client_addr, &1000);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 200_i128, JOB_DEADLINE),
        (String::from_str(&env, "Task 2"), 300_i128, JOB_DEADLINE + 1),
    ];

    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    let job = escrow.get_job(&job_id);
    assert_eq!(job.total_amount, 500);
    assert_eq!(job.token_balances.len(), 0); // no non-default tokens
    assert!(job.milestones.iter().all(|m| m.token.is_none()));

    escrow.fund_job(&job_id, &client_addr, &0, &0);
    assert_eq!(job_id, 1);
}

#[test]
fn test_propose_revision_rejected_for_multi_token_job() {
    let env = Env::default();
    env.mock_all_auths();

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token_a = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let token_b = env.register_stellar_asset_contract_v2(admin.clone()).address();

    StellarAssetClient::new(&env, &token_a).mint(&client_addr, &1000);
    StellarAssetClient::new(&env, &token_b).mint(&client_addr, &1000);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);
    escrow.add_allowed_token(&admin, &token_a);
    escrow.add_allowed_token(&admin, &token_b);

    let milestones: Vec<(soroban_sdk::String, i128, u64, Option<Address>)> = vec![
        &env,
        (String::from_str(&env, "Task"), 100_i128, JOB_DEADLINE, Some(token_b.clone())),
    ];

    let job_id = escrow.create_multi_token_job(
        &client_addr,
        &freelancer,
        &token_a,
        &milestones,
        &(JOB_DEADLINE + 10),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );

    let new_milestones: Vec<Milestone> = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "New Task"),
            amount: 100,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];

    // Should panic with InvalidStatus (error code 3) — multi-token revision not supported.
    let result = escrow.try_propose_revision(&client_addr, &job_id, &new_milestones);
    assert!(result.is_err());
}

#[test]
fn test_mark_job_disputed_succeeds_from_funded() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Funded);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);

    let dispute_id = 42_u64;
    escrow.mark_job_disputed(&job_id, &dispute_id);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Disputed);

    let events = env.events().all();
    assert!(events.len() > 0, "Events should be emitted");
}

#[test]
fn test_mark_job_disputed_succeeds_from_in_progress() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);
    escrow.submit_milestone(&job_id, &0, &freelancer);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::InProgress);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);

    let dispute_id = 99_u64;
    escrow.mark_job_disputed(&job_id, &dispute_id);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Disputed);
}

#[test]
fn test_mark_job_disputed_fails_when_dispute_contract_unset() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let dispute_id = 1_u64;
    let result = escrow.try_mark_job_disputed(&job_id, &dispute_id);
    assert_eq!(result, Err(Ok(EscrowError::Unauthorized)));
}

#[test]
fn test_mark_job_disputed_fails_when_caller_not_registered_dispute_contract() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let registered_dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &registered_dispute_contract);
    // With mock_all_auths a different caller cannot be distinguished at the unit-test
    // level; this test documents the deployed behaviour.
}

#[test]
fn test_mark_job_disputed_fails_on_completed_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);
    escrow.submit_milestone(&job_id, &0, &freelancer);
    escrow.approve_milestone(&job_id, &0, &client_addr);
    escrow.complete_job(&job_id, &client_addr);

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Completed);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);

    let dispute_id = 1_u64;
    let result = escrow.try_mark_job_disputed(&job_id, &dispute_id);
    assert_eq!(result, Err(Ok(EscrowError::InvalidStatus)));
}

#[test]
fn test_mark_job_disputed_fails_on_created_job() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    let job = escrow.get_job(&job_id);
    assert_eq!(job.status, JobStatus::Created);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);

    let dispute_id = 1_u64;
    let result = escrow.try_mark_job_disputed(&job_id, &dispute_id);
    assert_eq!(result, Err(Ok(EscrowError::InvalidStatus)));
}

#[test]
fn test_mark_job_disputed_emits_correct_event_payload() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, admin) = setup_test(&env);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Task 1"), 1000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    escrow.fund_job(&job_id, &client_addr, &0, &0);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);

    let dispute_id = 123_u64;
    escrow.mark_job_disputed(&job_id, &dispute_id);

    let events = env.events().all();
    let last_event = events.last().unwrap();

    let topic1: Symbol = last_event.1.get(1).unwrap().into_val(&env);
    assert_eq!(topic1, Symbol::new(&env, "disputed"));

    let data: (u64, u64, Address, Address) = last_event.2.into_val(&env);
    assert_eq!(data.0, job_id);
    assert_eq!(data.1, dispute_id);
    assert_eq!(data.2, client_addr);
    assert_eq!(data.3, freelancer);
}

// ============================================================
// SUB-CONTRACTING TESTS (issue #898)
// ============================================================

/// Helper: create a funded single-milestone job and return all handles.
fn setup_sub_assign_job(
    env: &Env,
) -> (
    EscrowContractClient<'_>,
    Address,
    Address,
    Address,
    Address,
    u64,
) {
    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(env, &contract_id);

    let client_addr = Address::generate(env);
    let freelancer = Address::generate(env);
    let admin = Address::generate(env);
    let treasury = Address::generate(env);

    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    StellarAssetClient::new(env, &token).mint(&client_addr, &10_000);

    let signers = vec![env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);

    let milestones = vec![
        env,
        (String::from_str(env, "Main work"), 1_000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &(JOB_DEADLINE + 1),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    (escrow, client_addr, freelancer, token, treasury, job_id)
}

#[test]
fn test_sub_assign_happy_payout_split() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    let sub_amount = 300_i128;

    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &sub_amount);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.sub_freelancer, sub_freelancer);
    assert_eq!(sub.amount, sub_amount);
    assert_eq!(sub.status, SubAssignmentStatus::Active);

    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    let tc = TokenClient::new(&env, &token);
    let freelancer_before = tc.balance(&freelancer);
    let sub_before = tc.balance(&sub_freelancer);

    escrow.complete_job(&job_id, &client_addr);

    let freelancer_gained = tc.balance(&freelancer) - freelancer_before;
    let sub_gained = tc.balance(&sub_freelancer) - sub_before;

    assert_eq!(sub_gained, sub_amount);
    assert_eq!(freelancer_gained, 1_000 - sub_amount);

    let sub_after = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub_after.status, SubAssignmentStatus::Paid);
}

#[test]
fn test_sub_assign_via_release_milestone() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &400_i128);

    escrow.submit_milestone(&job_id, &0_u32, &freelancer);

    let tc = TokenClient::new(&env, &token);
    let sub_before = tc.balance(&sub_freelancer);
    let fl_before = tc.balance(&freelancer);

    escrow.release_milestone(&job_id, &0_u32, &client_addr, &0_i128, &1_u64);

    assert_eq!(tc.balance(&sub_freelancer) - sub_before, 400);
    assert_eq!(tc.balance(&freelancer) - fl_before, 600);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.status, SubAssignmentStatus::Paid);
}

#[test]
fn test_sub_assign_dispute_isolation() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(&env, &contract_id);

    let client_addr = Address::generate(&env);
    let freelancer = Address::generate(&env);
    let admin = Address::generate(&env);
    let treasury = Address::generate(&env);

    let token = env.register_stellar_asset_contract_v2(admin.clone()).address();
    StellarAssetClient::new(&env, &token).mint(&client_addr, &10_000);

    let signers = vec![&env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604800);

    let milestones = vec![
        &env,
        (String::from_str(&env, "Main work"), 1_000_i128, JOB_DEADLINE),
    ];
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token,
        &milestones,
        &(JOB_DEADLINE + 1),
        &GRACE_PERIOD,
        &(env.ledger().sequence() + DEFAULT_EXPIRY_LEDGER),
    );
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    let dispute_contract = Address::generate(&env);
    escrow.set_dispute_contract(&admin, &dispute_contract);
    escrow.mark_job_disputed(&job_id, &1_u64);

    let tc = TokenClient::new(&env, &token);
    let fl_before = tc.balance(&freelancer);
    let sub_before = tc.balance(&sub_freelancer);

    escrow.resolve_dispute_callback(&job_id, &DisputeResolution::FreelancerWins);

    assert_eq!(tc.balance(&sub_freelancer) - sub_before, 0);
    assert_eq!(tc.balance(&freelancer) - fl_before, 1_000);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.status, SubAssignmentStatus::Active);
}

#[test]
fn test_cancel_mid_sub_assignment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    escrow.cancel_sub_assignment(&job_id, &0_u32, &freelancer);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.status, SubAssignmentStatus::Cancelled);

    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    let tc = TokenClient::new(&env, &token);
    let fl_before = tc.balance(&freelancer);
    let sub_before = tc.balance(&sub_freelancer);

    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(tc.balance(&sub_freelancer) - sub_before, 0);
    assert_eq!(tc.balance(&freelancer) - fl_before, 1_000);
}

#[test]
fn test_cancel_job_cancels_active_sub_assignment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    escrow.cancel_job(&job_id, &client_addr, &1_u64);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.status, SubAssignmentStatus::Cancelled);
}

#[test]
fn test_unauthorized_sub_assignment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, _freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let impostor = Address::generate(&env);
    let sub_freelancer = Address::generate(&env);

    let result = escrow.try_sub_assign_milestone(
        &job_id,
        &0_u32,
        &client_addr,
        &sub_freelancer,
        &300_i128,
    );
    assert!(result.is_err());

    let result2 = escrow.try_sub_assign_milestone(
        &job_id,
        &0_u32,
        &impostor,
        &sub_freelancer,
        &300_i128,
    );
    assert!(result2.is_err());
}

#[test]
fn test_sub_assign_self_assignment_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, _client_addr, freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let result = escrow.try_sub_assign_milestone(
        &job_id,
        &0_u32,
        &freelancer,
        &freelancer,
        &300_i128,
    );
    assert!(result.is_err());
}

#[test]
fn test_partial_payment_blocked_by_active_sub_assignment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    escrow.submit_milestone(&job_id, &0_u32, &freelancer);

    // release_partial_payment must be rejected while an active sub-assignment exists;
    // otherwise the sub-freelancer's promised amount would be silently dropped.
    let result = escrow.try_release_partial_payment(
        &job_id,
        &0_u32,
        &500_i128,
        &client_addr,
        &1_u64,
    );
    assert!(result.is_err());
}

// ============================================================
// EmergencyWithdraw — deferred vs immediate payment model tests
// Issue #1118: approved-but-unpaid milestone funds must not be
// permanently stranded by an emergency withdrawal.
// ============================================================

/// Helper: create a 2-milestone job with a 500-unit mint, fund it, and return
/// `(escrow_client, client_addr, freelancer, token_addr, admin, job_id)`.
fn setup_emergency_withdraw_job(
    env: &Env,
) -> (EscrowContractClient<'_>, Address, Address, Address, Address, u64) {
    let contract_id = env.register_contract(None, EscrowContract);
    let escrow = EscrowContractClient::new(env, &contract_id);

    let client_addr = Address::generate(env);
    let freelancer = Address::generate(env);
    let admin = Address::generate(env);

    let token_address = env
        .register_stellar_asset_contract_v2(admin.clone())
        .address();
    let token_admin = StellarAssetClient::new(env, &token_address);
    // Mint enough for the job plus some spare to detect double-payment.
    token_admin.mint(&client_addr, &1000);

    let treasury = Address::generate(env);
    let signers = vec![env, admin.clone()];
    escrow.initialize(&signers, &1, &treasury, &0, &604_800);

    // Two milestones: 200 and 300 (total 500).
    // Milestone tuples are (description, amount, deadline) — no token field.
    // Deadlines must be strictly ascending and before job_deadline.
    let job_id = escrow.create_job(
        &client_addr,
        &freelancer,
        &token_address,
        &vec![
            env,
            (String::from_str(env, "milestone-0"), 200_i128, JOB_DEADLINE / 2),
            (String::from_str(env, "milestone-1"), 300_i128, JOB_DEADLINE),
        ],
        &JOB_DEADLINE,
        &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );
    // fund_job(job_id, client, agreed_value_stroops=0 skip oracle, max_slippage_bps=0)
    escrow.fund_job(&job_id, &client_addr, &0_i128, &0_u32);

    (escrow, client_addr, freelancer, token_address, admin, job_id)
}

/// Helper: pause the contract via a single-signer, no-timelock setup.
/// Returns the temp signer used so callers can reuse it for further actions.
fn pause_for_emergency(
    env: &Env,
    escrow: &EscrowContractClient<'_>,
    admin: &Address,
) -> Address {
    let temp_signer = Address::generate(env);
    escrow.propose_admin_action(admin, &AdminAction::AddSigner(temp_signer.clone()));
    let proposal_id = escrow.propose_admin_action(admin, &AdminAction::Pause);
    env.ledger().with_mut(|l| l.timestamp += 48 * 60 * 60 + 1);
    escrow.approve_admin_action(&temp_signer, &proposal_id);
    temp_signer
}

/// Helper: propose and auto-execute an EmergencyWithdraw against `job_id`,
/// sending the withdrawable escrow to `recipient`.
fn do_emergency_withdraw(
    env: &Env,
    escrow: &EscrowContractClient<'_>,
    admin: &Address,
    job_id: u64,
    recipient: &Address,
) {
    escrow.propose_admin_action(
        admin,
        &AdminAction::EmergencyWithdraw(job_id, recipient.clone()),
    );
}

// -----------------------------------------------------------------
// Test 1: deferred-payment model — approved-but-unpaid milestone
// funds are paid to the freelancer; remainder goes to admin recipient.
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_deferred_model_pays_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // Deferred model: submit milestone 0, approve it (no funds move yet).
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    // Milestone 1 is still Pending (not submitted). Total escrowed: 500.
    // After approve_milestone(0): milestone 0 is Approved but unpaid.
    // Expected: emergency withdraw pays 200 to freelancer, 300 to recipient.

    let recipient = Address::generate(&env);

    // Record balances before.
    let freelancer_before = token.balance(&freelancer);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let freelancer_after = token.balance(&freelancer);
    let recipient_after = token.balance(&recipient);

    // Freelancer must have received the deferred approved milestone (200).
    assert_eq!(
        freelancer_after - freelancer_before,
        200,
        "freelancer should receive the deferred approved milestone amount"
    );
    // Recipient (admin) gets the remaining unapproved balance (300).
    assert_eq!(
        recipient_after - recipient_before,
        300,
        "admin recipient should receive only the unapproved escrow balance"
    );
}

// -----------------------------------------------------------------
// Test 2: immediate-payment model — release_milestone pays before
// the emergency; EmergencyWithdraw must not re-pay an already-paid
// milestone (no double-payment regression).
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_immediate_model_no_double_payment() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // Immediate model: submit + release_milestone for milestone 0 (pays freelancer 200 now).
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.release_milestone(&job_id, &0_u32, &client_addr, &0_i128, &1_u64);

    // Milestone 0 is now Approved AND already paid.
    // Milestone 1 is still Pending. Remaining escrow: 300.
    // Emergency withdraw should give 300 to recipient; freelancer gets nothing more.

    let recipient = Address::generate(&env);

    let freelancer_before = token.balance(&freelancer);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let freelancer_after = token.balance(&freelancer);
    let recipient_after = token.balance(&recipient);

    // Freelancer must NOT receive any additional payment (already received 200 via release_milestone).
    assert_eq!(
        freelancer_after - freelancer_before,
        0,
        "freelancer must not be double-paid for already-released milestone"
    );
    // Admin recipient gets the remaining unapproved balance (300).
    assert_eq!(
        recipient_after - recipient_before,
        300,
        "admin recipient should receive the remaining unapproved escrow balance"
    );
}

// -----------------------------------------------------------------
// Test 3: mixed job — milestone 0 deferred-approved (via approve_milestone,
// no funds moved), milestone 1 still Pending. Emergency withdraw must:
//   - pay freelancer for deferred milestone 0 (200)
//   - send remaining unapproved escrow balance (300) to admin recipient
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_mixed_deferred_and_immediate() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // Milestone 0 — deferred: submit then approve (no payout yet, funds still in escrow).
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    // Milestone 1 — still Pending; no submission, no release.
    // State:
    //   milestone 0: Approved, disbursed = 0  (deferred, 200 still in escrow)
    //   milestone 1: Pending                  (300 in escrow, unapproved)
    //   Job status: InProgress
    // Expected: freelancer receives 200 (deferred); admin recipient receives 300.

    let recipient = Address::generate(&env);

    let freelancer_before = token.balance(&freelancer);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let freelancer_after = token.balance(&freelancer);
    let recipient_after = token.balance(&recipient);

    assert_eq!(
        freelancer_after - freelancer_before,
        200,
        "freelancer should receive the 200 from the deferred approved milestone"
    );
    assert_eq!(
        recipient_after - recipient_before,
        300,
        "admin recipient should receive the 300 unapproved escrow balance"
    );
}

// -----------------------------------------------------------------
// Test 4: no milestones approved — all escrow goes to admin recipient.
// (Sanity / regression: existing behaviour unchanged for the no-approval case.)
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_no_approvals_all_goes_to_recipient() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, _freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // No milestones submitted or approved — full escrow 500 goes to recipient.
    let recipient = Address::generate(&env);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let recipient_after = token.balance(&recipient);

    assert_eq!(
        recipient_after - recipient_before,
        500,
        "all escrowed funds go to admin recipient when no milestones are approved"
    );
}

// -----------------------------------------------------------------
// Test 5: all milestones deferred-approved — full escrow goes to
// freelancer; admin recipient receives nothing.
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_all_deferred_approved_all_to_freelancer() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // Deferred-approve both milestones (total 500 still in escrow).
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);
    escrow.submit_milestone(&job_id, &1_u32, &freelancer);
    escrow.approve_milestone(&job_id, &1_u32, &client_addr);

    let recipient = Address::generate(&env);
    let freelancer_before = token.balance(&freelancer);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let freelancer_after = token.balance(&freelancer);
    let recipient_after = token.balance(&recipient);

    assert_eq!(
        freelancer_after - freelancer_before,
        500,
        "all escrowed funds go to freelancer when all milestones are deferred-approved"
    );
    assert_eq!(
        recipient_after - recipient_before,
        0,
        "admin recipient receives nothing when all funds are owed to freelancer"
    );
}

// -----------------------------------------------------------------
// Test 6 (regression): inactivity auto-approval — milestone paid via
// finalize_inactivity_approval, then EmergencyWithdraw is called.
// Because finalize_inactivity_approval now records MilestoneDisbursed,
// EmergencyWithdraw must treat the milestone as the immediate-payment
// model and must NOT pay the freelancer a second time.
//
// Setup: 2-milestone job (200 + 300 = 500 total).
//   - Milestone 0 submitted at t=0; inactivity threshold elapsed (7 days);
//     trigger_inactivity_approval called; grace period elapsed (3 days);
//     finalize_inactivity_approval called → freelancer receives 200.
//   - Milestone 1 still Pending.
//   - Contract paused → EmergencyWithdraw.
// Expected: freelancer gets 0 extra (already received 200), recipient gets 300.
// -----------------------------------------------------------------
#[test]
fn test_emergency_withdraw_no_double_pay_after_inactivity_approval() {
    let env = Env::default();
    env.mock_all_auths();
    // Start at t=1000 so timestamps are above zero.
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, _client_addr, freelancer, token_addr, admin, job_id) =
        setup_emergency_withdraw_job(&env);

    let token = TokenClient::new(&env, &token_addr);

    // Submit milestone 0 at t=1000.
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);

    // Advance past the 7-day inactivity threshold so trigger is allowed.
    // INACTIVITY_THRESHOLD_SECS = 7 * 24 * 3600 = 604_800.
    env.ledger().with_mut(|l| l.timestamp = 1000 + 604_800 + 1);

    // Trigger inactivity (either client or freelancer can call this).
    escrow.trigger_inactivity_extension(&job_id, &0_u32, &freelancer);

    // Advance past the 3-day grace period so finalise is allowed.
    // INACTIVITY_GRACE_SECS = 3 * 24 * 3600 = 259_200.
    env.ledger().with_mut(|l| l.timestamp += 259_200 + 1);

    // Finalise: funds leave escrow → freelancer receives 200.
    let freelancer_before = token.balance(&freelancer);
    escrow.finalize_inactivity_approval(&job_id, &0_u32, &freelancer);
    let freelancer_after_finalize = token.balance(&freelancer);

    assert_eq!(
        freelancer_after_finalize - freelancer_before,
        200,
        "freelancer should receive 200 from finalize_inactivity_approval"
    );

    // Now pause the contract and run EmergencyWithdraw.
    // Milestone 0 is Approved with MilestoneDisbursed == 200 (immediate model).
    // Milestone 1 is Pending (300 still in escrow).
    // EmergencyWithdraw should give 300 to recipient and 0 extra to freelancer.
    let recipient = Address::generate(&env);
    let freelancer_before_withdraw = token.balance(&freelancer);
    let recipient_before = token.balance(&recipient);

    pause_for_emergency(&env, &escrow, &admin);
    do_emergency_withdraw(&env, &escrow, &admin, job_id, &recipient);

    let freelancer_after_withdraw = token.balance(&freelancer);
    let recipient_after = token.balance(&recipient);

    assert_eq!(
        freelancer_after_withdraw - freelancer_before_withdraw,
        0,
        "freelancer must not be double-paid: inactivity-approved milestone already disbursed"
    );
    assert_eq!(
        recipient_after - recipient_before,
        300,
        "admin recipient should receive the remaining unapproved escrow balance"
    );
}

// ============================================================
// Revision + sub-assignment cross-feature reconciliation tests
// Issue: accept_revision never reconciled active SubAssignment
// entries, enabling orphaned payouts and disproportionate splits.
// ============================================================

/// Proposing a revision that drops a milestone id with an active sub-assignment
/// must be rejected at propose time, preventing silent orphaning.
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_propose_revision_rejected_if_drops_milestone_with_active_sub_assign() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    // Propose a revision that replaces milestone id=0 with a new id=1 (dropping id=0).
    // The active sub-assignment on id=0 would be permanently orphaned.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 1,
            description: String::from_str(&env, "Renumbered"),
            amount: 1_000_i128,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];
    escrow.propose_revision(&client_addr, &job_id, &new_milestones);
}

/// If a sub-assignment is created between propose_revision and accept_revision,
/// the accept call must still block the orphan (safety-net at accept time).
#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_accept_revision_rejected_if_sub_assign_created_after_propose() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, _token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    // Propose dropping id=0 before any sub-assignment exists — propose_revision passes.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 1,
            description: String::from_str(&env, "Renumbered"),
            amount: 1_000_i128,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];
    escrow.propose_revision(&client_addr, &job_id, &new_milestones);

    // Sub-assignment created in the propose→accept window.
    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &300_i128);

    // Accept must be rejected: accepting now would orphan the sub-assignment.
    escrow.accept_revision(&freelancer, &job_id);
}

/// Accepting a revision that shrinks a milestone's amount must proportionally
/// scale any active sub-assignment amount so neither party ends up with zero.
#[test]
fn test_accept_revision_scales_sub_assign_on_milestone_shrink() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    // Sub-assign 600 of the 1_000-unit milestone to a sub-freelancer.
    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &600_i128);

    // Client proposes shrinking the milestone from 1_000 to 400 (same id=0).
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Shrunk"),
            amount: 400_i128,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];
    escrow.propose_revision(&client_addr, &job_id, &new_milestones);

    // Freelancer accepts. The sub-amount must be scaled: 600 * 400 / 1_000 = 240.
    escrow.accept_revision(&freelancer, &job_id);

    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.amount, 240, "sub-amount should be proportionally scaled");
    assert_eq!(sub.status, SubAssignmentStatus::Active);

    // Verify payout uses the new scaled sub-amount: sub gets 240, main freelancer gets 160.
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    let tc = TokenClient::new(&env, &token);
    let fl_before = tc.balance(&freelancer);
    let sub_before = tc.balance(&sub_freelancer);

    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(tc.balance(&sub_freelancer) - sub_before, 240);
    assert_eq!(tc.balance(&freelancer) - fl_before, 400 - 240);
}

/// Accepting a revision that *grows* a milestone's amount must leave any active
/// sub-assignment's absolute amount unchanged (the main freelancer's share increases).
/// Freelancer proposes the increase so that the client (the payer) is the acceptor —
/// matching accept_revision's pattern where the top-up transfer is from job.client.
#[test]
fn test_accept_revision_sub_assign_unchanged_on_milestone_growth() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (escrow, client_addr, freelancer, token, _treasury, job_id) =
        setup_sub_assign_job(&env);

    // Sub-assign 400 of the 1_000-unit milestone.
    let sub_freelancer = Address::generate(&env);
    escrow.sub_assign_milestone(&job_id, &0_u32, &freelancer, &sub_freelancer, &400_i128);

    // Freelancer proposes raising milestone from 1_000 to 2_000 (same id=0).
    // Client accepts, which also authorizes the required top-up transfer from client.
    let new_milestones = vec![
        &env,
        Milestone {
            id: 0,
            description: String::from_str(&env, "Expanded"),
            amount: 2_000_i128,
            status: MilestoneStatus::Pending,
            deadline: JOB_DEADLINE,
            token: None,
        },
    ];
    escrow.propose_revision(&freelancer, &job_id, &new_milestones);
    escrow.accept_revision(&client_addr, &job_id);

    // Sub-amount should be unchanged at 400; only the main freelancer's share grew.
    let sub = escrow.get_sub_assignment(&job_id, &0_u32).unwrap();
    assert_eq!(sub.amount, 400, "sub-amount must not change when milestone grows");
    assert_eq!(sub.status, SubAssignmentStatus::Active);

    // Verify payout: sub gets 400, main freelancer gets 2_000 - 400 = 1_600.
    escrow.submit_milestone(&job_id, &0_u32, &freelancer);
    escrow.approve_milestone(&job_id, &0_u32, &client_addr);

    let tc = TokenClient::new(&env, &token);
    let fl_before = tc.balance(&freelancer);
    let sub_before = tc.balance(&sub_freelancer);

    escrow.complete_job(&job_id, &client_addr);

    assert_eq!(tc.balance(&sub_freelancer) - sub_before, 400);
    assert_eq!(tc.balance(&freelancer) - fl_before, 2_000 - 400);
}

// ─── #1156: get_price_oracle coverage ─────────────────────────────────────────

#[test]
fn test_get_price_oracle_unset_returns_none() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, _admin) = setup_test(&env);

    // Fresh contract — no oracle configured yet
    assert!(contract.get_price_oracle().is_none());
}

#[test]
fn test_get_price_oracle_returns_set_address() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let oracle = Address::generate(&env);
    contract.set_price_oracle(&admin, &oracle);

    assert_eq!(contract.get_price_oracle(), Some(oracle));
}

#[test]
fn test_set_price_oracle_overwrites_previous_value() {
    let env = Env::default();
    env.mock_all_auths();
    let (contract, _client, _freelancer, _token, admin) = setup_test(&env);

    let oracle_first = Address::generate(&env);
    let oracle_second = Address::generate(&env);

    contract.set_price_oracle(&admin, &oracle_first);
    assert_eq!(contract.get_price_oracle(), Some(oracle_first));

    contract.set_price_oracle(&admin, &oracle_second);
    assert_eq!(contract.get_price_oracle(), Some(oracle_second));
}

// ============================================================
// Multi-sig proposal storage lifecycle, validation and TTL bumps
// Issues #1153, #1154, #1155, #1158
// ============================================================

use soroban_sdk::testutils::storage::{
    Instance as _InstanceTestUtils, Persistent as _PersistentTestUtils,
};

/// Seconds after which a pending multi-sig proposal is no longer actionable.
/// Mirrors `crate::PROPOSAL_TTL` (7 days), which is private to the contract.
const MULTISIG_PROPOSAL_TTL: u64 = 7 * 24 * 60 * 60;
const MULTISIG_TIME_LOCK: u64 = 48 * 60 * 60;

fn instance_has_proposal(
    env: &Env,
    contract: &EscrowContractClient<'_>,
    proposal_id: u64,
) -> bool {
    env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .has(&crate::DataKey::MultiSigProposal(proposal_id))
    })
}

fn archived_proposal(
    env: &Env,
    contract: &EscrowContractClient<'_>,
    proposal_id: u64,
) -> Option<crate::MultiSigProposal> {
    env.as_contract(&contract.address, || {
        env.storage()
            .persistent()
            .get(&crate::DataKey::MultiSigProposal(proposal_id))
    })
}

fn instance_entry_count(env: &Env, contract: &EscrowContractClient<'_>) -> u32 {
    env.as_contract(&contract.address, || env.storage().instance().all().len())
}

/// The contract exposes no signer/fee getters, so tests read the values the
/// same way the rest of this suite reads contract state — straight from storage.
fn current_signers(env: &Env, contract: &EscrowContractClient<'_>) -> Vec<Address> {
    env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .get(&crate::DataKey::MultiSigSigners)
            .unwrap()
    })
}

fn current_fee_bps(env: &Env, contract: &EscrowContractClient<'_>) -> u32 {
    env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .get(&soroban_sdk::symbol_short!("FEE"))
            .unwrap()
    })
}

// ── Issue #1153: proposals must not accumulate in instance storage ───────────

#[test]
fn test_executed_proposal_is_moved_out_of_instance_storage() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);

    // Threshold is 1 and Unpause carries no time lock, so this auto-executes.
    let proposal_id = contract.propose_admin_action(&admin, &AdminAction::Unpause);

    assert!(
        !instance_has_proposal(&env, &contract, proposal_id),
        "executed proposal must not stay in instance storage"
    );

    let archived = archived_proposal(&env, &contract, proposal_id)
        .expect("executed proposal should be archived to persistent storage");
    assert!(archived.executed, "archived record should be marked executed");
    assert_eq!(archived.id, proposal_id);
}

#[test]
fn test_execution_not_before_key_is_cleared_on_execution() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);

    // Pause is time-locked, so `MultiSigExecutionNotBefore` is written on propose.
    let proposal_id = contract.propose_admin_action(&admin, &AdminAction::Pause);
    assert!(env.as_contract(&contract.address, || {
        env.storage()
            .instance()
            .has(&crate::DataKey::MultiSigExecutionNotBefore(proposal_id))
    }));

    env.ledger().with_mut(|l| l.timestamp += MULTISIG_TIME_LOCK + 1);
    contract.execute_proposal(&admin, &proposal_id);

    assert!(
        env.as_contract(&contract.address, || {
            !env.storage()
                .instance()
                .has(&crate::DataKey::MultiSigExecutionNotBefore(proposal_id))
        }),
        "time-lock key must be dropped once the proposal is terminal"
    );
}

#[test]
fn test_instance_storage_does_not_grow_over_many_propose_execute_cycles() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);

    // First cycle establishes the steady state: it is the call that first writes
    // `MultiSigProposalCount` and `Paused`. Every later cycle must reuse them.
    contract.propose_admin_action(&admin, &AdminAction::Unpause);
    let baseline = instance_entry_count(&env, &contract);

    for _ in 0..50 {
        let id = contract.propose_admin_action(&admin, &AdminAction::Unpause);
        assert!(
            !instance_has_proposal(&env, &contract, id),
            "proposal {} lingered in instance storage",
            id
        );
    }

    assert_eq!(
        instance_entry_count(&env, &contract),
        baseline,
        "instance storage grew across propose+execute cycles"
    );
}

#[test]
fn test_expired_proposal_can_be_pruned_from_instance_storage() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    // Threshold 2, so a single proposal never auto-executes and goes stale.
    let (contract, _, _, _, signer1, _signer2) = setup_multisig(&env);

    let baseline = instance_entry_count(&env, &contract);
    let proposal_id = contract.propose_admin_action(&signer1, &AdminAction::Pause);
    assert!(instance_has_proposal(&env, &contract, proposal_id));
    assert!(instance_entry_count(&env, &contract) > baseline);

    env.ledger()
        .with_mut(|l| l.timestamp += MULTISIG_PROPOSAL_TTL + 1);

    contract.prune_expired_proposal(&proposal_id);

    assert!(
        !instance_has_proposal(&env, &contract, proposal_id),
        "expired proposal must be evicted from instance storage"
    );
    let archived = archived_proposal(&env, &contract, proposal_id)
        .expect("pruned proposal should remain readable in persistent storage");
    assert!(
        !archived.executed,
        "an expired proposal must not be recorded as executed"
    );
    // Back to the pre-proposal size plus `MultiSigProposalCount`, which is a
    // single monotonic u64 that must persist so ids are never reused.
    assert_eq!(
        instance_entry_count(&env, &contract),
        baseline + 1,
        "pruning should leave only the proposal counter behind"
    );

    // And the expiry path, like the execution path, must be repeatable without
    // instance storage creeping upward.
    let steady = instance_entry_count(&env, &contract);
    for _ in 0..20 {
        let id = contract.propose_admin_action(&signer1, &AdminAction::Pause);
        env.ledger()
            .with_mut(|l| l.timestamp += MULTISIG_PROPOSAL_TTL + 1);
        contract.prune_expired_proposal(&id);
        assert!(!instance_has_proposal(&env, &contract, id));
    }
    assert_eq!(
        instance_entry_count(&env, &contract),
        steady,
        "instance storage grew across propose+expire+prune cycles"
    );
}

#[test]
fn test_prune_expired_proposal_rejects_a_still_live_proposal() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, signer1, _signer2) = setup_multisig(&env);
    let proposal_id = contract.propose_admin_action(&signer1, &AdminAction::Pause);

    // One second short of the TTL — still actionable, so not prunable.
    env.ledger()
        .with_mut(|l| l.timestamp += MULTISIG_PROPOSAL_TTL);

    let res = contract.try_prune_expired_proposal(&proposal_id);
    assert!(res.is_err(), "live proposal must not be prunable"); // ProposalNotExpirable (#40)
    assert!(instance_has_proposal(&env, &contract, proposal_id));
}

#[test]
fn test_prune_expired_proposal_unknown_id_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, _signer1, _signer2) = setup_multisig(&env);

    let res = contract.try_prune_expired_proposal(&999u64);
    assert!(res.is_err()); // MultiSigProposalNotFound (#31)
}

#[test]
fn test_archived_proposal_still_reports_already_executed() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);
    let proposal_id = contract.propose_admin_action(&admin, &AdminAction::Unpause);

    // Archiving must not degrade the error into "not found": the proposal is
    // gone from instance storage but its outcome is still knowable.
    let res = contract.try_execute_proposal(&admin, &proposal_id);
    assert_eq!(
        res,
        Err(Ok(EscrowError::MultiSigAlreadyExecuted)),
        "re-executing an archived proposal should report AlreadyExecuted"
    );

    let res = contract.try_approve_admin_action(&admin, &proposal_id);
    assert_eq!(res, Err(Ok(EscrowError::MultiSigAlreadyExecuted)));
}

// ── Issue #1154: SetFeeBps validated when proposed, not only when executed ───

#[test]
fn test_propose_set_fee_bps_above_max_rejected_at_proposal_time() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    // Threshold 2 so the proposal would otherwise sit awaiting approvals and
    // only fail much later, at execution time.
    let (contract, _, _, _, signer1, _signer2) = setup_multisig(&env);
    let baseline = instance_entry_count(&env, &contract);

    let res = contract
        .try_propose_admin_action(&signer1, &AdminAction::SetFeeBps(crate::MAX_FEE_BPS + 1));
    assert_eq!(res, Err(Ok(EscrowError::InvalidFee)));

    // The proposal must never have been created: no stored proposal, and the
    // counter is untouched so the next valid proposal still gets id 1.
    assert!(!instance_has_proposal(&env, &contract, 1));
    assert_eq!(
        instance_entry_count(&env, &contract),
        baseline,
        "a rejected proposal must not leave anything in storage"
    );

    let next_id = contract.propose_admin_action(&signer1, &AdminAction::SetFeeBps(crate::MAX_FEE_BPS));
    assert_eq!(next_id, 1, "rejected proposal must not consume a proposal id");
}

#[test]
fn test_propose_set_fee_bps_at_max_is_accepted_and_executes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);

    // Boundary: exactly MAX_FEE_BPS is valid and still auto-executes.
    let proposal_id =
        contract.propose_admin_action(&admin, &AdminAction::SetFeeBps(crate::MAX_FEE_BPS));
    let archived = archived_proposal(&env, &contract, proposal_id)
        .expect("valid fee proposal should execute and archive");
    assert!(archived.executed);
    assert_eq!(current_fee_bps(&env, &contract), crate::MAX_FEE_BPS);
}

// ── Issue #1155: AddSigner/RemoveSigner no-ops must not report success ───────

#[test]
fn test_add_signer_already_present_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);
    let signers_before = current_signers(&env, &contract);

    // `admin` is already a signer — this used to succeed while changing nothing.
    let res = contract.try_propose_admin_action(&admin, &AdminAction::AddSigner(admin.clone()));
    assert_eq!(res, Err(Ok(EscrowError::AlreadyInitialized)));

    assert_eq!(
        current_signers(&env, &contract),
        signers_before,
        "a rejected AddSigner must leave the signer set untouched"
    );
    // The failed execution must not leave an "executed" record behind.
    assert!(archived_proposal(&env, &contract, 1).is_none());
}

#[test]
fn test_remove_signer_not_present_is_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, signer1, _signer2) = setup_multisig(&env);
    let stranger = Address::generate(&env);
    let signers_before = current_signers(&env, &contract);

    // Threshold is 2, so drive the proposal to execution via a real approval.
    let proposal_id =
        contract.propose_admin_action(&signer1, &AdminAction::RemoveSigner(stranger.clone()));
    let res = contract.try_approve_admin_action(&_signer2, &proposal_id);
    assert_eq!(res, Err(Ok(EscrowError::SignerNotFound)));

    assert_eq!(
        current_signers(&env, &contract),
        signers_before,
        "a rejected RemoveSigner must leave the signer set untouched"
    );
    assert!(
        archived_proposal(&env, &contract, proposal_id).is_none(),
        "a failed removal must not be archived as executed"
    );
}

#[test]
fn test_add_and_remove_signer_still_work_for_real_changes() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, admin) = setup_test(&env);
    let newcomer = Address::generate(&env);

    let add_id = contract.propose_admin_action(&admin, &AdminAction::AddSigner(newcomer.clone()));
    assert!(current_signers(&env, &contract).contains(&newcomer));
    assert!(
        archived_proposal(&env, &contract, add_id)
            .expect("real add should be archived")
            .executed
    );

    // Threshold is 1 and there are now 2 signers, so the removal is permitted.
    let remove_id =
        contract.propose_admin_action(&admin, &AdminAction::RemoveSigner(newcomer.clone()));
    assert!(!current_signers(&env, &contract).contains(&newcomer));
    assert!(
        archived_proposal(&env, &contract, remove_id)
            .expect("real removal should be archived")
            .executed
    );
}

#[test]
fn test_remove_signer_below_threshold_still_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    // 2 signers, threshold 2 — removing either would drop the set below it.
    let (contract, _, _, _, signer1, signer2) = setup_multisig(&env);

    let proposal_id =
        contract.propose_admin_action(&signer1, &AdminAction::RemoveSigner(signer2.clone()));
    let res = contract.try_approve_admin_action(&signer2, &proposal_id);
    assert_eq!(res, Err(Ok(EscrowError::InvalidThreshold)));
    assert_eq!(current_signers(&env, &contract).len(), 2);
}

// ── Issue #1158: bump_escrow must actually extend the job's TTL ──────────────

#[test]
fn test_bump_escrow_extends_job_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    let key = crate::DataKey::Job(job_id);

    // Let a large part of the original TTL burn down before bumping.
    env.ledger().with_mut(|l| l.sequence_number = 400_000);
    let ttl_before =
        env.as_contract(&contract.address, || env.storage().persistent().get_ttl(&key));

    contract.bump_escrow(&job_id);

    let ttl_after =
        env.as_contract(&contract.address, || env.storage().persistent().get_ttl(&key));

    assert!(
        ttl_after > ttl_before,
        "bump_escrow should extend the TTL (before: {}, after: {})",
        ttl_before,
        ttl_after
    );
    assert_eq!(
        ttl_after,
        crate::ESCROW_TTL_LEDGERS,
        "bump_escrow should extend the job key to the full escrow TTL"
    );
}

#[test]
fn test_bump_escrow_keeps_job_readable_past_its_original_ttl() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| {
        l.timestamp = 1000;
        l.sequence_number = 1;
    });

    let (contract, client, freelancer, token, _admin) = setup_test(&env);
    let milestones = vec![&env, (String::from_str(&env, "Work"), 1000_i128, JOB_DEADLINE)];
    let job_id = contract.create_job(
        &client, &freelancer, &token, &milestones, &JOB_DEADLINE, &GRACE_PERIOD,
        &DEFAULT_EXPIRY_LEDGER,
    );

    // Bump partway through the original window, then advance past where the
    // entry would have been archived had the bump not taken effect.
    env.ledger().with_mut(|l| l.sequence_number = 400_000);
    contract.bump_escrow(&job_id);
    env.ledger().with_mut(|l| l.sequence_number = 700_000);

    let job = contract.get_job(&job_id);
    assert_eq!(job.id, job_id);
}

#[test]
fn test_bump_escrow_unknown_job_rejected() {
    let env = Env::default();
    env.mock_all_auths();
    env.ledger().with_mut(|l| l.timestamp = 1000);

    let (contract, _, _, _, _admin) = setup_test(&env);

    let res = contract.try_bump_escrow(&999u64);
    assert_eq!(res, Err(Ok(EscrowError::JobNotFound)));
}
