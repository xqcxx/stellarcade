#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, token, Address, Env,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const PRECISION: i128 = 1_000_000_000_000; // 1e12

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
    InvalidAmount = 4,
    Overflow = 5,
    InsufficientBalance = 6,
}

// ---------------------------------------------------------------------------
// Storage Keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    StakingToken,
    RewardToken,
    GlobalState,
    Position(Address),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalState {
    pub total_staked: i128,
    pub last_update_timestamp: u64,
    pub reward_per_share_acc: i128,
    pub reward_rate: i128, // reward per second
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserPosition {
    pub amount: i128,
    pub reward_debt: i128,
    pub pending_rewards: i128,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[contractevent]
pub struct Staked {
    #[topic]
    pub user: Address,
    pub amount: i128,
}

#[contractevent]
pub struct Unstaked {
    #[topic]
    pub user: Address,
    pub amount: i128,
}

#[contractevent]
pub struct RewardsClaimed {
    #[topic]
    pub user: Address,
    pub amount: i128,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct Staking;

#[contractimpl]
impl Staking {
    /// Initialise the staking contract.
    pub fn init(
        env: Env,
        admin: Address,
        staking_token: Address,
        reward_token: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::StakingToken, &staking_token);
        env.storage()
            .instance()
            .set(&DataKey::RewardToken, &reward_token);

        let state = GlobalState {
            total_staked: 0,
            last_update_timestamp: env.ledger().timestamp(),
            reward_per_share_acc: 0,
            reward_rate: 0,
        };
        env.storage().instance().set(&DataKey::GlobalState, &state);

        Ok(())
    }

    /// Set the reward rate (admin only).
    pub fn set_reward_rate(env: Env, admin: Address, rate: i128) -> Result<(), Error> {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotInitialized)?;
        admin.require_auth();
        if admin != stored_admin {
            return Err(Error::NotAuthorized);
        }

        Self::update_pool(&env)?;

        let mut state: GlobalState = env.storage().instance().get(&DataKey::GlobalState).unwrap();
        state.reward_rate = rate;
        env.storage().instance().set(&DataKey::GlobalState, &state);

        Ok(())
    }

    /// Stake tokens to earn rewards.
    pub fn stake(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        user.require_auth();

        Self::update_pool(&env)?;

        let mut state: GlobalState = env.storage().instance().get(&DataKey::GlobalState).unwrap();
        let mut position: UserPosition = env
            .storage()
            .persistent()
            .get(&DataKey::Position(user.clone()))
            .unwrap_or(UserPosition {
                amount: 0,
                reward_debt: 0,
                pending_rewards: 0,
            });

        // Calculate pending rewards before updating position
        if position.amount > 0 {
            let pending = (position.amount * state.reward_per_share_acc / PRECISION) - position.reward_debt;
            position.pending_rewards += pending;
        }

        // Transfer tokens from user
        let staking_token: Address = env.storage().instance().get(&DataKey::StakingToken).unwrap();
        let token_client = token::Client::new(&env, &staking_token);
        token_client.transfer(&user, &env.current_contract_address(), &amount);

        // Update position and state
        position.amount += amount;
        position.reward_debt = position.amount * state.reward_per_share_acc / PRECISION;
        state.total_staked += amount;

        env.storage().persistent().set(&DataKey::Position(user.clone()), &position);
        env.storage().instance().set(&DataKey::GlobalState, &state);

        Staked { user: user.clone(), amount }.publish(&env);

        Ok(())
    }

    /// Withdraw staked tokens and claim rewards.
    pub fn unstake(env: Env, user: Address, amount: i128) -> Result<(), Error> {
        user.require_auth();
        
        let mut position: UserPosition = env
            .storage()
            .persistent()
            .get(&DataKey::Position(user.clone()))
            .ok_or(Error::InvalidAmount)?;

        if amount > position.amount || amount < 0 {
            return Err(Error::InvalidAmount);
        }

        Self::update_pool(&env)?;

        let mut state: GlobalState = env.storage().instance().get(&DataKey::GlobalState).unwrap();

        // Calculate pending rewards
        let pending = (position.amount * state.reward_per_share_acc / PRECISION) - position.reward_debt;
        position.pending_rewards += pending;

        // Update position and state
        position.amount -= amount;
        position.reward_debt = if position.amount > 0 {
            position.amount * state.reward_per_share_acc / PRECISION
        } else {
            0
        };
        state.total_staked -= amount;

        // Transfer tokens back to user
        let staking_token: Address = env.storage().instance().get(&DataKey::StakingToken).unwrap();
        let token_client = token::Client::new(&env, &staking_token);
        token_client.transfer(&env.current_contract_address(), &user, &amount);

        env.storage().persistent().set(&DataKey::Position(user.clone()), &position);
        env.storage().instance().set(&DataKey::GlobalState, &state);

        Unstaked { user: user.clone(), amount }.publish(&env);

        Ok(())
    }

    /// Claim accrued rewards.
    pub fn claim_rewards(env: Env, user: Address) -> Result<i128, Error> {
        user.require_auth();

        Self::update_pool(&env)?;

        let state: GlobalState = env.storage().instance().get(&DataKey::GlobalState).unwrap();
        let mut position: UserPosition = env
            .storage()
            .persistent()
            .get(&DataKey::Position(user.clone()))
            .ok_or(Error::InvalidAmount)?;

        let pending = (position.amount * state.reward_per_share_acc / PRECISION) - position.reward_debt;
        let total_claimable = position.pending_rewards + pending;

        if total_claimable <= 0 {
            return Ok(0);
        }

        // Reset user rewards
        position.pending_rewards = 0;
        position.reward_debt = position.amount * state.reward_per_share_acc / PRECISION;

        // Transfer reward tokens
        let reward_token_addr: Address = env.storage().instance().get(&DataKey::RewardToken).unwrap();
        let token_client = token::Client::new(&env, &reward_token_addr);
        token_client.transfer(&env.current_contract_address(), &user, &total_claimable);

        env.storage().persistent().set(&DataKey::Position(user.clone()), &position);

        RewardsClaimed { user: user.clone(), amount: total_claimable }.publish(&env);

        Ok(total_claimable)
    }

    /// View user position.
    pub fn position_of(env: Env, user: Address) -> UserPosition {
        let mut position: UserPosition = env
            .storage()
            .persistent()
            .get(&DataKey::Position(user.clone()))
            .unwrap_or(UserPosition {
                amount: 0,
                reward_debt: 0,
                pending_rewards: 0,
            });

        // Calculate dynamic pending rewards for the view call
        if let Some(mut state) = env.storage().instance().get::<_, GlobalState>(&DataKey::GlobalState) {
            let timestamp = env.ledger().timestamp();
            if timestamp > state.last_update_timestamp && state.total_staked > 0 {
                let duration = (timestamp - state.last_update_timestamp) as i128;
                let rewards = duration * state.reward_rate;
                state.reward_per_share_acc += rewards * PRECISION / state.total_staked;
            }
            let pending = (position.amount * state.reward_per_share_acc / PRECISION) - position.reward_debt;
            position.pending_rewards += pending;
        }

        position
    }

    // -----------------------------------------------------------------------
    // Internal Helpers
    // -----------------------------------------------------------------------

    fn update_pool(env: &Env) -> Result<(), Error> {
        let mut state: GlobalState = env.storage().instance().get(&DataKey::GlobalState).ok_or(Error::NotInitialized)?;
        let timestamp = env.ledger().timestamp();

        if timestamp <= state.last_update_timestamp {
            return Ok(());
        }

        if state.total_staked > 0 {
            let duration = (timestamp - state.last_update_timestamp) as i128;
            let rewards = duration * state.reward_rate;
            state.reward_per_share_acc += rewards * PRECISION / state.total_staked;
        }

        state.last_update_timestamp = timestamp;
        env.storage().instance().set(&DataKey::GlobalState, &state);

        Ok(())
    }

    fn require_initialized(env: &Env) -> Result<Address, Error> {
        env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotInitialized)
    }
}

// = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = =
// Tests
// = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = = =

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, token, Address, Env};

    struct Setup {
        env: Env,
        client: StakingClient<'static>,
        admin: Address,
        user1: Address,
        user2: Address,
        staking_token: token::StellarAssetClient<'static>,
        reward_token: token::StellarAssetClient<'static>,
        staking_token_addr: Address,
        reward_token_addr: Address,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(Staking, ());
        let client = StakingClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let user1 = Address::generate(&env);
        let user2 = Address::generate(&env);

        let staking_token_addr = env.register_stellar_asset_contract(Address::generate(&env));
        let reward_token_addr = env.register_stellar_asset_contract(Address::generate(&env));

        let staking_token = token::StellarAssetClient::new(&env, &staking_token_addr);
        let reward_token = token::StellarAssetClient::new(&env, &reward_token_addr);

        client.init(&admin, &staking_token_addr, &reward_token_addr);

        // SAFETY: client borrows env by reference
        let client: StakingClient<'static> = unsafe { core::mem::transmute(client) };

        Setup {
            env,
            client,
            admin,
            user1,
            user2,
            staking_token,
            reward_token,
            staking_token_addr,
            reward_token_addr,
        }
    }

    #[test]
    fn test_init_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(Staking, ());
        let client = StakingClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let staking_token = Address::generate(&env);
        let reward_token = Address::generate(&env);

        client.init(&admin, &staking_token, &reward_token);
    }

    #[test]
    fn test_init_twice_fails() {
        let s = setup();
        let result = s.client.try_init(&s.admin, &s.staking_token_addr, &s.reward_token_addr);
        assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
    }

    #[test]
    fn test_stake_and_unstake() {
        let s = setup();
        let amount = 1000i128;

        s.staking_token.mint(&s.user1, &amount);
        s.client.stake(&s.user1, &amount);

        let pos = s.client.position_of(&s.user1);
        assert_eq!(pos.amount, amount);

        s.client.unstake(&s.user1, &amount);
        let pos = s.client.position_of(&s.user1);
        assert_eq!(pos.amount, 0);
    }

    #[test]
    fn test_reward_accrual() {
        let s = setup();
        let stake_amount = 1000i128;
        let rate = 10i128; // 10 reward tokens per second

        s.client.set_reward_rate(&s.admin, &rate);

        s.staking_token.mint(&s.user1, &stake_amount);
        s.client.stake(&s.user1, &stake_amount);

        // Advance time by 10 seconds
        s.env.ledger().set_timestamp(s.env.ledger().timestamp() + 10);

        let pos = s.client.position_of(&s.user1);
        // Expected rewards: 10 seconds * 10 rate = 100 rewards
        assert_eq!(pos.pending_rewards, 100i128);
    }

    #[test]
    fn test_claim_rewards() {
        let s = setup();
        let stake_amount = 1000i128;
        let rate = 10i128;

        // Fund contract with rewards
        s.reward_token.mint(&s.client.address, &10000i128);

        s.client.set_reward_rate(&s.admin, &rate);

        s.staking_token.mint(&s.user1, &stake_amount);
        s.client.stake(&s.user1, &stake_amount);

        // Advance time by 100 seconds (1000 rewards)
        s.env.ledger().set_timestamp(s.env.ledger().timestamp() + 100);

        let claimed = s.client.claim_rewards(&s.user1);
        assert_eq!(claimed, 1000i128);

        let reward_bal = token::Client::new(&s.env, &s.reward_token_addr).balance(&s.user1);
        assert_eq!(reward_bal, 1000i128);
    }

    #[test]
    fn test_multiple_users_fair_distribution() {
        let s = setup();
        let rate = 100i128;

        s.reward_token.mint(&s.client.address, &1_000_000i128);
        s.client.set_reward_rate(&s.admin, &rate);

        // User 1 stakes 1000
        s.staking_token.mint(&s.user1, &1000i128);
        s.client.stake(&s.user1, &1000i128);

        // Advance 10 seconds (1000 rewards accrued to User 1)
        s.env.ledger().set_timestamp(s.env.ledger().timestamp() + 10);

        // User 2 stakes 1000
        s.staking_token.mint(&s.user2, &1000i128);
        s.client.stake(&s.user2, &1000i128);

        // Advance 10 seconds (1000 rewards split between User 1 and User 2)
        s.env.ledger().set_timestamp(s.env.ledger().timestamp() + 10);

        let pos1 = s.client.position_of(&s.user1);
        let pos2 = s.client.position_of(&s.user2);

        // User 1: 1000 (first 10s) + 500 (next 10s) = 1500
        // User 2: 500 (next 10s) = 500
        assert_eq!(pos1.pending_rewards, 1500i128);
        assert_eq!(pos2.pending_rewards, 500i128);
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Contract, #4)")]
    fn test_unstake_excessive_amount() {
        let s = setup();
        s.staking_token.mint(&s.user1, &100i128);
        s.client.stake(&s.user1, &100i128);
        s.client.unstake(&s.user1, &101i128);
    }
}
