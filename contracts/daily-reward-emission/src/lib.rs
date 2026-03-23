#![no_std]

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype,
    token, Address, Env, Symbol,
};

// ── Storage Keys ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    RewardPool,
    Schedule(Symbol),              // schedule_id → EmissionConfig
    EpochState(Symbol),            // schedule_id → EpochState
    Claimed(Symbol, u64, Address), // (schedule_id, epoch_id, user)
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmissionConfig {
    pub schedule_id: Symbol,
    /// Reward per epoch in stroops.
    pub rewards_per_epoch: i128,
    /// Epoch duration in ledger seconds.
    pub epoch_duration: u64,
    /// Token address used for rewards.
    pub token: Address,
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmissionEpochState {
    pub current_epoch: u64,
    pub epoch_start_time: u64,
    pub total_emitted: i128,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct EmissionConfigured {
    #[topic]
    pub schedule_id: Symbol,
    pub rewards_per_epoch: i128,
}

#[contractevent]
pub struct EpochEmitted {
    #[topic]
    pub schedule_id: Symbol,
    pub epoch_id: u64,
    pub amount: i128,
}

#[contractevent]
pub struct RewardClaimed {
    #[topic]
    pub schedule_id: Symbol,
    pub epoch_id: u64,
    pub user: Address,
    pub amount: i128,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct DailyRewardEmission;

#[contractimpl]
impl DailyRewardEmission {
    /// Initialize with admin and reward pool address.
    pub fn init(env: Env, admin: Address, reward_pool_contract: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::RewardPool, &reward_pool_contract);
    }

    /// Configure or update an emission schedule. Admin-only.
    pub fn configure_emission(env: Env, schedule_id: Symbol, config: EmissionConfig) {
        Self::require_admin(&env);
        assert!(config.rewards_per_epoch > 0, "Rewards per epoch must be positive");
        assert!(config.epoch_duration > 0, "Epoch duration must be positive");

        let epoch_state = EmissionEpochState {
            current_epoch: 0,
            epoch_start_time: env.ledger().timestamp(),
            total_emitted: 0,
        };

        env.storage().persistent().set(&DataKey::Schedule(schedule_id.clone()), &config);
        env.storage()
            .persistent()
            .set(&DataKey::EpochState(schedule_id.clone()), &epoch_state);

        EmissionConfigured {
            schedule_id,
            rewards_per_epoch: config.rewards_per_epoch,
        }
        .publish(&env);
    }

    /// Finalize the current epoch and advance to the next. Admin-only.
    /// Emits rewards from the reward pool into the contract for distribution.
    pub fn emit_for_epoch(env: Env, schedule_id: Symbol) -> u64 {
        Self::require_admin(&env);

        let config: EmissionConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Schedule(schedule_id.clone()))
            .expect("Schedule not found");

        assert!(config.active, "Schedule is inactive");

        let mut epoch_state: EmissionEpochState = env
            .storage()
            .persistent()
            .get(&DataKey::EpochState(schedule_id.clone()))
            .expect("Epoch state not found");

        let now = env.ledger().timestamp();
        assert!(
            now >= epoch_state.epoch_start_time + config.epoch_duration,
            "Epoch not yet complete"
        );

        // Advance epoch
        epoch_state.current_epoch = epoch_state.current_epoch.checked_add(1).expect("Overflow");
        epoch_state.epoch_start_time = now;
        epoch_state.total_emitted = epoch_state
            .total_emitted
            .checked_add(config.rewards_per_epoch)
            .expect("Overflow");

        // Pull rewards from pool into this contract
        let pool: Address =
            env.storage().instance().get(&DataKey::RewardPool).expect("Not initialized");
        let token_client = token::Client::new(&env, &config.token);
        token_client.transfer(&pool, &env.current_contract_address(), &config.rewards_per_epoch);

        env.storage()
            .persistent()
            .set(&DataKey::EpochState(schedule_id.clone()), &epoch_state);

        let epoch_id = epoch_state.current_epoch;
        EpochEmitted { schedule_id, epoch_id, amount: config.rewards_per_epoch }.publish(&env);

        epoch_id
    }

    /// Claim a daily reward for a specific epoch. User must not have claimed before.
    pub fn claim_daily_reward(
        env: Env,
        user: Address,
        schedule_id: Symbol,
        epoch_id: u64,
        reward_amount: i128,
    ) {
        user.require_auth();
        assert!(reward_amount > 0, "Reward amount must be positive");

        let claimed_key = DataKey::Claimed(schedule_id.clone(), epoch_id, user.clone());
        assert!(
            !env.storage().persistent().has(&claimed_key),
            "Reward already claimed"
        );

        let config: EmissionConfig = env
            .storage()
            .persistent()
            .get(&DataKey::Schedule(schedule_id.clone()))
            .expect("Schedule not found");

        // Mark as claimed before transfer (reentrancy guard)
        env.storage().persistent().set(&claimed_key, &true);

        // Transfer reward to user
        let token_client = token::Client::new(&env, &config.token);
        token_client.transfer(&env.current_contract_address(), &user, &reward_amount);

        RewardClaimed { schedule_id, epoch_id, user, amount: reward_amount }.publish(&env);
    }

    /// Read the current emission state for a schedule.
    pub fn emission_state(env: Env, epoch_id: Symbol) -> EmissionEpochState {
        env.storage()
            .persistent()
            .get(&DataKey::EpochState(epoch_id))
            .expect("Schedule not found")
    }

    // ── Internal ─────────────────────────────────────────────────
    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Not initialized");
        admin.require_auth();
    }
}

// ── Tests ─────────────────────────────────────────────────────────
#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Ledger, LedgerInfo},
        token::{Client as TokenClient, StellarAssetClient},
        Env, Symbol,
    };

    fn setup_token<'a>(env: &Env, admin: &Address) -> (Address, StellarAssetClient<'a>, TokenClient<'a>) {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let addr = sac.address();
        (addr.clone(), StellarAssetClient::new(env, &addr), TokenClient::new(env, &addr))
    }

    #[test]
    fn test_configure_and_emit() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let pool = Address::generate(&env);
        let user = Address::generate(&env);

        let (token_id, sa, tc) = setup_token(&env, &admin);
        sa.mint(&pool, &10_000);

        let contract_id = env.register_contract(None, DailyRewardEmission);
        let client = DailyRewardEmissionClient::new(&env, &contract_id);

        client.init(&admin, &pool);

        let schedule_id = Symbol::new(&env, "daily");
        let config = EmissionConfig {
            schedule_id: schedule_id.clone(),
            rewards_per_epoch: 1000,
            epoch_duration: 86400,
            token: token_id.clone(),
            active: true,
        };

        // Set ledger time
        env.ledger().set(LedgerInfo {
            timestamp: 1000,
            protocol_version: 25,
            sequence_number: 1,
            network_id: [0u8; 32],
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: 1_000_000,
        });

        client.configure_emission(&schedule_id, &config);

        // Advance time by 1 epoch
        env.ledger().set(LedgerInfo {
            timestamp: 88000,
            protocol_version: 25,
            sequence_number: 2,
            network_id: [0u8; 32],
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: 1_000_000,
        });

        client.emit_for_epoch(&schedule_id);
        assert_eq!(tc.balance(&contract_id), 1000);

        // Claim
        client.claim_daily_reward(&user, &schedule_id, &1, &100);
        assert_eq!(tc.balance(&user), 100);
    }

    #[test]
    #[should_panic(expected = "Reward already claimed")]
    fn test_double_claim_fails() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let pool = Address::generate(&env);
        let user = Address::generate(&env);

        let (token_id, sa, _) = setup_token(&env, &admin);
        sa.mint(&pool, &10_000);

        let contract_id = env.register_contract(None, DailyRewardEmission);
        let client = DailyRewardEmissionClient::new(&env, &contract_id);
        client.init(&admin, &pool);

        let sid = Symbol::new(&env, "d");
        let config = EmissionConfig {
            schedule_id: sid.clone(),
            rewards_per_epoch: 500,
            epoch_duration: 1,
            token: token_id.clone(),
            active: true,
        };

        env.ledger().set(LedgerInfo {
            timestamp: 1,
            protocol_version: 25,
            sequence_number: 1,
            network_id: [0u8; 32],
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: 1_000_000,
        });

        client.configure_emission(&sid, &config);

        env.ledger().set(LedgerInfo {
            timestamp: 10,
            protocol_version: 25,
            sequence_number: 2,
            network_id: [0u8; 32],
            base_reserve: 10,
            min_temp_entry_ttl: 1,
            min_persistent_entry_ttl: 1,
            max_entry_ttl: 1_000_000,
        });

        client.emit_for_epoch(&sid);
        client.claim_daily_reward(&user, &sid, &1, &50);
        client.claim_daily_reward(&user, &sid, &1, &50); // should panic
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();
        let admin = Address::generate(&env);
        let pool = Address::generate(&env);
        let contract_id = env.register_contract(None, DailyRewardEmission);
        let client = DailyRewardEmissionClient::new(&env, &contract_id);
        client.init(&admin, &pool);
        client.init(&admin, &pool);
    }
}
