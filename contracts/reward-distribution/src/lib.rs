#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN, Env,
};

// ---------------------------------------------------------------------------
// TTL / storage constants
// ---------------------------------------------------------------------------

/// ~30 days at 5 s / ledger
const PERSISTENT_BUMP_LEDGERS: u32 = 518_400;
/// Threshold at which a persistent entry is renewed (~7 days from expiry)
const PERSISTENT_BUMP_THRESHOLD: u32 = PERSISTENT_BUMP_LEDGERS - 100_800;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    NotAuthorized = 3,
    CampaignNotFound = 4,
    CampaignAlreadyExists = 5,
    CampaignExhausted = 6,
    CampaignNotActive = 7,
    NothingToClaim = 8,
    AlreadyClaimed = 9,
    InvalidAmount = 10,
    Overflow = 11,
}

// ---------------------------------------------------------------------------
// Storage keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    /// Contract-level config — stored in instance storage
    Admin,
    TreasuryContract,
    BalanceContract,
    /// Per-campaign state — persistent, keyed by campaign_id
    Campaign(u32),
    /// Accrued reward for (campaign, user) before claim — persistent
    Accrued(u32, Address),
    /// Claim flag for (campaign, user) — persistent (reentrancy guard)
    Claimed(u32, Address),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CampaignStatus {
    Active = 0,
    Exhausted = 1,
    Closed = 2,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct CampaignData {
    /// SHA-256 of off-chain rules document
    pub rules_hash: BytesN<32>,
    /// Total budget allocated when the campaign was defined
    pub budget: i128,
    /// Remaining distributable balance (budget − already accrued)
    pub remaining: i128,
    pub status: CampaignStatus,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[contractevent]
pub struct ContractInitialized {
    #[topic]
    pub admin: Address,
    pub treasury_contract: Address,
    pub balance_contract: Address,
}

#[contractevent]
pub struct CampaignDefined {
    #[topic]
    pub campaign_id: u32,
    pub budget: i128,
}

#[contractevent]
pub struct RewardAccrued {
    #[topic]
    pub campaign_id: u32,
    pub user: Address,
    pub amount: i128,
    pub new_total: i128,
}

#[contractevent]
pub struct RewardClaimed {
    #[topic]
    pub campaign_id: u32,
    pub user: Address,
    pub amount: i128,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct RewardDistribution;

#[contractimpl]
impl RewardDistribution {
    // -----------------------------------------------------------------------
    // Initialisation
    // -----------------------------------------------------------------------

    /// Initialise the contract.  Can only be called once.
    ///
    /// * `admin`             — privileged account for campaign management.
    /// * `treasury_contract` — address of the on-chain treasury holding
    ///                         campaign budgets (stored for composability).
    /// * `balance_contract`  — address of the token / balance contract used
    ///                         to settle claims.
    pub fn init(
        env: Env,
        admin: Address,
        treasury_contract: Address,
        balance_contract: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::TreasuryContract, &treasury_contract);
        env.storage()
            .instance()
            .set(&DataKey::BalanceContract, &balance_contract);

        ContractInitialized { admin, treasury_contract, balance_contract }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Campaign management
    // -----------------------------------------------------------------------

    /// Define a new reward campaign.  Admin only.
    ///
    /// * `campaign_id` — unique numeric identifier.
    /// * `rules_hash`  — SHA-256 of the off-chain eligibility rules document.
    /// * `budget`      — maximum tokens distributable; must be > 0.
    pub fn define_reward_campaign(
        env: Env,
        campaign_id: u32,
        rules_hash: BytesN<32>,
        budget: i128,
    ) -> Result<(), Error> {
        let admin = Self::require_initialized(&env)?;
        admin.require_auth();

        if budget <= 0 {
            return Err(Error::InvalidAmount);
        }

        let key = DataKey::Campaign(campaign_id);
        if env.storage().persistent().has(&key) {
            return Err(Error::CampaignAlreadyExists);
        }

        let campaign = CampaignData {
            rules_hash,
            budget,
            remaining: budget,
            status: CampaignStatus::Active,
        };

        env.storage().persistent().set(&key, &campaign);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        CampaignDefined { campaign_id, budget }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reward accrual
    // -----------------------------------------------------------------------

    /// Record a pending reward for `user`.  Admin only.
    ///
    /// The call is additive — repeated calls accumulate until the user claims.
    /// The campaign's `remaining` balance is decremented immediately to uphold
    /// the invariant `Σ accrued ≤ budget`.
    pub fn accrue_reward(
        env: Env,
        user: Address,
        campaign_id: u32,
        amount: i128,
    ) -> Result<(), Error> {
        let admin = Self::require_initialized(&env)?;
        admin.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Load and validate campaign
        let campaign_key = DataKey::Campaign(campaign_id);
        let mut campaign: CampaignData = env
            .storage()
            .persistent()
            .get(&campaign_key)
            .ok_or(Error::CampaignNotFound)?;

        if campaign.status != CampaignStatus::Active {
            return Err(Error::CampaignNotActive);
        }

        let new_remaining = campaign
            .remaining
            .checked_sub(amount)
            .ok_or(Error::Overflow)?;
        if new_remaining < 0 {
            return Err(Error::CampaignExhausted);
        }

        // Accumulate user's pending balance
        let accrued_key = DataKey::Accrued(campaign_id, user.clone());
        let current_accrued: i128 = env
            .storage()
            .persistent()
            .get(&accrued_key)
            .unwrap_or(0i128);
        let new_accrued = current_accrued.checked_add(amount).ok_or(Error::Overflow)?;

        // Commit campaign state
        campaign.remaining = new_remaining;
        if campaign.remaining == 0 {
            campaign.status = CampaignStatus::Exhausted;
        }

        env.storage().persistent().set(&campaign_key, &campaign);
        env.storage().persistent().extend_ttl(
            &campaign_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // Commit accrued balance
        env.storage().persistent().set(&accrued_key, &new_accrued);
        env.storage().persistent().extend_ttl(
            &accrued_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        RewardAccrued { campaign_id, user, amount, new_total: new_accrued }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Claim
    // -----------------------------------------------------------------------

    /// Claim all accrued rewards for `user` in a campaign.
    ///
    /// * The user must authenticate.
    /// * The reentrancy guard (`Claimed` flag) is set **before** any external
    ///   settlement call.
    /// * Returns the amount of tokens claimed.
    pub fn claim_reward(env: Env, user: Address, campaign_id: u32) -> Result<i128, Error> {
        Self::require_initialized(&env)?;
        user.require_auth();

        // Duplicate-claim guard
        let claimed_key = DataKey::Claimed(campaign_id, user.clone());
        if env.storage().persistent().has(&claimed_key) {
            return Err(Error::AlreadyClaimed);
        }

        let accrued_key = DataKey::Accrued(campaign_id, user.clone());
        let accrued: i128 = env
            .storage()
            .persistent()
            .get(&accrued_key)
            .unwrap_or(0i128);

        if accrued <= 0 {
            return Err(Error::NothingToClaim);
        }

        // ── Reentrancy guard: set Claimed BEFORE any external call ──────────
        env.storage().persistent().set(&claimed_key, &true);
        env.storage().persistent().extend_ttl(
            &claimed_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // Zero out the accrued balance
        env.storage().persistent().set(&accrued_key, &0i128);
        env.storage().persistent().extend_ttl(
            &accrued_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // ── Placeholder for cross-contract token transfer ────────────────────
        // In production: balance_contract.transfer(user, accrued)
        // Stored for composability — the balance_contract address is available
        // via `env.storage().instance().get(&DataKey::BalanceContract)`.

        RewardClaimed { campaign_id, user, amount: accrued }.publish(&env);

        Ok(accrued)
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    /// Return the current state of a campaign, or `None` if it does not exist.
    pub fn campaign_state(env: Env, campaign_id: u32) -> Option<CampaignData> {
        env.storage()
            .persistent()
            .get(&DataKey::Campaign(campaign_id))
    }

    /// Return the unclaimed accrued balance for `user` in a campaign.
    pub fn accrued_for(env: Env, user: Address, campaign_id: u32) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Accrued(campaign_id, user))
            .unwrap_or(0i128)
    }

    /// Return whether `user` has already claimed from `campaign_id`.
    pub fn has_claimed(env: Env, user: Address, campaign_id: u32) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::Claimed(campaign_id, user))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn require_initialized(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

    // ── Helpers ─────────────────────────────────────────────────────────────

    fn rules_hash(env: &Env) -> BytesN<32> {
        BytesN::from_array(env, &[0u8; 32])
    }

    struct Setup {
        env: Env,
        client: RewardDistributionClient<'static>,
        admin: Address,
        treasury: Address,
        balance: Address,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(RewardDistribution, ());
        let client = RewardDistributionClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let balance = Address::generate(&env);

        client.init(&admin, &treasury, &balance);

        // SAFETY: client borrows env by reference; env is moved into Setup and
        // lives as long as all accesses through client.
        let client: RewardDistributionClient<'static> = unsafe { core::mem::transmute(client) };

        Setup {
            env,
            client,
            admin,
            treasury,
            balance,
        }
    }

    // ── init ────────────────────────────────────────────────────────────────

    #[test]
    fn test_init_succeeds() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(RewardDistribution, ());
        let client = RewardDistributionClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let treasury = Address::generate(&env);
        let balance = Address::generate(&env);

        client.init(&admin, &treasury, &balance);
    }

    #[test]
    fn test_init_twice_fails() {
        let s = setup();
        let result = s.client.try_init(&s.admin, &s.treasury, &s.balance);
        assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
    }

    // ── define_reward_campaign ───────────────────────────────────────────────

    #[test]
    fn test_define_campaign_succeeds() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &1_000i128);

        let state = s.client.campaign_state(&1u32).unwrap();
        assert_eq!(state.budget, 1_000);
        assert_eq!(state.remaining, 1_000);
        assert_eq!(state.status, CampaignStatus::Active);
    }

    #[test]
    fn test_define_campaign_duplicate_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &500i128);
        let result = s.client.try_define_reward_campaign(&1u32, &hash, &500i128);
        assert_eq!(result, Err(Ok(Error::CampaignAlreadyExists)));
    }

    #[test]
    fn test_define_campaign_zero_budget_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        let result = s.client.try_define_reward_campaign(&1u32, &hash, &0i128);
        assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    }

    #[test]
    fn test_define_campaign_negative_budget_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        let result = s.client.try_define_reward_campaign(&1u32, &hash, &(-1i128));
        assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    }

    // ── accrue_reward ────────────────────────────────────────────────────────

    #[test]
    fn test_accrue_succeeds_and_accumulates() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &1_000i128);

        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &1u32, &300i128);
        s.client.accrue_reward(&user, &1u32, &200i128);

        assert_eq!(s.client.accrued_for(&user, &1u32), 500i128);

        let state = s.client.campaign_state(&1u32).unwrap();
        assert_eq!(state.remaining, 500i128);
    }

    #[test]
    fn test_accrue_exhausts_campaign() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&2u32, &hash, &100i128);

        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &2u32, &100i128);

        let state = s.client.campaign_state(&2u32).unwrap();
        assert_eq!(state.status, CampaignStatus::Exhausted);
        assert_eq!(state.remaining, 0);
    }

    #[test]
    fn test_accrue_over_budget_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&3u32, &hash, &50i128);

        let user = Address::generate(&s.env);
        let result = s.client.try_accrue_reward(&user, &3u32, &51i128);
        assert_eq!(result, Err(Ok(Error::CampaignExhausted)));
    }

    #[test]
    fn test_accrue_unknown_campaign_fails() {
        let s = setup();
        let user = Address::generate(&s.env);
        let result = s.client.try_accrue_reward(&user, &99u32, &10i128);
        assert_eq!(result, Err(Ok(Error::CampaignNotFound)));
    }

    #[test]
    fn test_accrue_zero_amount_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&4u32, &hash, &100i128);
        let user = Address::generate(&s.env);
        let result = s.client.try_accrue_reward(&user, &4u32, &0i128);
        assert_eq!(result, Err(Ok(Error::InvalidAmount)));
    }

    #[test]
    fn test_accrue_on_exhausted_campaign_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&5u32, &hash, &10i128);

        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &5u32, &10i128);

        let other = Address::generate(&s.env);
        let result = s.client.try_accrue_reward(&other, &5u32, &1i128);
        assert_eq!(result, Err(Ok(Error::CampaignNotActive)));
    }

    // ── claim_reward ─────────────────────────────────────────────────────────

    #[test]
    fn test_claim_succeeds() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &500i128);

        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &1u32, &250i128);

        let claimed = s.client.claim_reward(&user, &1u32);
        assert_eq!(claimed, 250i128);

        assert_eq!(s.client.accrued_for(&user, &1u32), 0i128);
        assert!(s.client.has_claimed(&user, &1u32));
    }

    #[test]
    fn test_claim_twice_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &500i128);

        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &1u32, &100i128);
        s.client.claim_reward(&user, &1u32);

        let result = s.client.try_claim_reward(&user, &1u32);
        assert_eq!(result, Err(Ok(Error::AlreadyClaimed)));
    }

    #[test]
    fn test_claim_nothing_accrued_fails() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &500i128);

        let user = Address::generate(&s.env);
        let result = s.client.try_claim_reward(&user, &1u32);
        assert_eq!(result, Err(Ok(Error::NothingToClaim)));
    }

    // ── queries ───────────────────────────────────────────────────────────────

    #[test]
    fn test_campaign_state_unknown_returns_none() {
        let s = setup();
        assert!(s.client.campaign_state(&999u32).is_none());
    }

    #[test]
    fn test_has_claimed_false_before_claim() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &100i128);
        let user = Address::generate(&s.env);
        s.client.accrue_reward(&user, &1u32, &50i128);
        assert!(!s.client.has_claimed(&user, &1u32));
    }

    // ── not-initialized guard ─────────────────────────────────────────────────

    #[test]
    fn test_define_campaign_without_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(RewardDistribution, ());
        let client = RewardDistributionClient::new(&env, &contract_id);
        let hash = BytesN::from_array(&env, &[0u8; 32]);
        let result = client.try_define_reward_campaign(&1u32, &hash, &100i128);
        assert_eq!(result, Err(Ok(Error::NotInitialized)));
    }

    #[test]
    fn test_accrue_without_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(RewardDistribution, ());
        let client = RewardDistributionClient::new(&env, &contract_id);
        let user = Address::generate(&env);
        let result = client.try_accrue_reward(&user, &1u32, &10i128);
        assert_eq!(result, Err(Ok(Error::NotInitialized)));
    }

    #[test]
    fn test_claim_without_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(RewardDistribution, ());
        let client = RewardDistributionClient::new(&env, &contract_id);
        let user = Address::generate(&env);
        let result = client.try_claim_reward(&user, &1u32);
        assert_eq!(result, Err(Ok(Error::NotInitialized)));
    }

    // ── multiple users ────────────────────────────────────────────────────────

    #[test]
    fn test_multiple_users_independent_accrual() {
        let s = setup();
        let hash = rules_hash(&s.env);
        s.client.define_reward_campaign(&1u32, &hash, &1_000i128);

        let alice = Address::generate(&s.env);
        let bob = Address::generate(&s.env);

        s.client.accrue_reward(&alice, &1u32, &400i128);
        s.client.accrue_reward(&bob, &1u32, &300i128);

        assert_eq!(s.client.accrued_for(&alice, &1u32), 400i128);
        assert_eq!(s.client.accrued_for(&bob, &1u32), 300i128);

        let alice_claimed = s.client.claim_reward(&alice, &1u32);
        assert_eq!(alice_claimed, 400i128);

        // Bob's balance is unaffected by Alice's claim
        assert_eq!(s.client.accrued_for(&bob, &1u32), 300i128);
        assert!(!s.client.has_claimed(&bob, &1u32));

        // Remaining was debited at accrue time, not claim time
        let campaign = s.client.campaign_state(&1u32).unwrap();
        assert_eq!(campaign.remaining, 300i128);
    }
}
