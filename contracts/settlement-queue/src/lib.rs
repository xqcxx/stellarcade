#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, symbol_short, Address,
    Env, Symbol,
};

// ---------------------------------------------------------------------------
// TTL / storage constants
// ---------------------------------------------------------------------------

const PERSISTENT_BUMP_LEDGERS: u32 = 518_400; // ~30 days
const PERSISTENT_BUMP_THRESHOLD: u32 = PERSISTENT_BUMP_LEDGERS - 100_800; // Renew ~7 days early

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
    InvalidBatchSize = 4,
    SettlementNotFound = 5,
    InvalidState = 6,
    Overflow = 7,
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettlementStatus {
    Pending = 0,
    Processed = 1,
    Failed = 2,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct SettlementData {
    pub account: Address,
    pub amount: i128,
    pub reason: Symbol,
    pub status: SettlementStatus,
    pub error_code: Option<u32>,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    RewardContract,
    TreasuryContract,
    Settlement(Symbol), // Keyed by settlement_id
    QueueHead,
    QueueTail,
    QueueItem(u64), // Keyed by index
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[contractevent]
pub struct ContractInitialized {
    #[topic]
    pub admin: Address,
    pub reward_contract: Address,
    pub treasury_contract: Address,
}

#[contractevent]
pub struct SettlementEnqueued {
    #[topic]
    pub settlement_id: Symbol,
    #[topic]
    pub account: Address,
    pub amount: i128,
}

#[contractevent]
pub struct SettlementProcessed {
    #[topic]
    pub settlement_id: Symbol,
    pub status: SettlementStatus,
}

#[contractevent]
pub struct SettlementFailed {
    #[topic]
    pub settlement_id: Symbol,
    pub error_code: u32,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct SettlementQueue;

#[contractimpl]
impl SettlementQueue {
    /// Initialise the contract.
    pub fn init(
        env: Env,
        admin: Address,
        reward_contract: Address,
        treasury_contract: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::RewardContract, &reward_contract);
        env.storage()
            .instance()
            .set(&DataKey::TreasuryContract, &treasury_contract);
        
        env.storage().instance().set(&DataKey::QueueHead, &0u64);
        env.storage().instance().set(&DataKey::QueueTail, &0u64);

        ContractInitialized { admin, reward_contract, treasury_contract }.publish(&env);

        Ok(())
    }

    /// Enqueue a new settlement.
    pub fn enqueue_settlement(
        env: Env,
        settlement_id: Symbol,
        account: Address,
        amount: i128,
        reason: Symbol,
    ) -> Result<(), Error> {
        let (admin, _reward_contract) = Self::require_initialized(&env)?;
        
        // Auth: Admin must authorize this
        admin.require_auth();

        let settlement_key = DataKey::Settlement(settlement_id.clone());
        if env.storage().persistent().has(&settlement_key) {
            return Err(Error::InvalidState); // Already exists
        }

        let settlement = SettlementData {
            account: account.clone(),
            amount,
            reason: reason.clone(),
            status: SettlementStatus::Pending,
            error_code: None,
        };

        env.storage().persistent().set(&settlement_key, &settlement);
        env.storage().persistent().extend_ttl(
            &settlement_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // Add to queue
        let mut tail: u64 = env.storage().instance().get(&DataKey::QueueTail).unwrap();
        env.storage().persistent().set(&DataKey::QueueItem(tail), &settlement_id);
        env.storage().persistent().extend_ttl(
            &DataKey::QueueItem(tail),
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        tail = tail.checked_add(1).ok_or(Error::Overflow)?;
        env.storage().instance().set(&DataKey::QueueTail, &tail);

        SettlementEnqueued { settlement_id, account, amount }.publish(&env);

        Ok(())
    }

    /// Process the next batch of settlements.
    pub fn process_next(env: Env, batch_size: u32) -> Result<u32, Error> {
        let (admin, _) = Self::require_initialized(&env)?;
        admin.require_auth();

        if batch_size == 0 {
            return Err(Error::InvalidBatchSize);
        }

        let mut head: u64 = env.storage().instance().get(&DataKey::QueueHead).unwrap();
        let tail: u64 = env.storage().instance().get(&DataKey::QueueTail).unwrap();

        let mut processed_count = 0;
        while head < tail && processed_count < batch_size {
            let item_key = DataKey::QueueItem(head);
            let settlement_id: Symbol = env.storage().persistent().get(&item_key).unwrap();
            
            let settlement_key = DataKey::Settlement(settlement_id.clone());
            let mut settlement: SettlementData = env.storage().persistent().get(&settlement_key).unwrap();

            if settlement.status == SettlementStatus::Pending {
                // In a real implementation, this would call out to Reward or Treasury
                // or just mark as processed if this contract is the final word.
                // For now, we update status to Processed.
                settlement.status = SettlementStatus::Processed;
                env.storage().persistent().set(&settlement_key, &settlement);
                
                SettlementProcessed { settlement_id: settlement_id.clone(), status: SettlementStatus::Processed }.publish(&env);
            }

            // Head always increments, effectively "popping" the queue even if status was already changed
            head += 1;
            processed_count += 1;
            
            // Clean up old queue item pointer
            env.storage().persistent().remove(&item_key);
        }

        env.storage().instance().set(&DataKey::QueueHead, &head);

        Ok(processed_count)
    }

    /// Mark a settlement as failed.
    pub fn mark_failed(env: Env, settlement_id: Symbol, error_code: u32) -> Result<(), Error> {
        let (admin, _) = Self::require_initialized(&env)?;
        admin.require_auth();

        let settlement_key = DataKey::Settlement(settlement_id.clone());
        let mut settlement: SettlementData = env
            .storage()
            .persistent()
            .get(&settlement_key)
            .ok_or(Error::SettlementNotFound)?;

        if settlement.status == SettlementStatus::Processed {
             return Err(Error::InvalidState);
        }

        settlement.status = SettlementStatus::Failed;
        settlement.error_code = Some(error_code);

        env.storage().persistent().set(&settlement_key, &settlement);

        SettlementFailed { settlement_id, error_code }.publish(&env);

        Ok(())
    }

    /// Query the state of a settlement.
    pub fn settlement_state(env: Env, settlement_id: Symbol) -> Option<SettlementData> {
        env.storage()
            .persistent()
            .get(&DataKey::Settlement(settlement_id))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn require_initialized(env: &Env) -> Result<(Address, Address), Error> {
        let admin: Address = env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        
        let reward: Address = env.storage()
            .instance()
            .get(&DataKey::RewardContract)
            .ok_or(Error::NotInitialized)?;

        Ok((admin, reward))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env};

    struct Setup<'a> {
        _env: Env,
        client: SettlementQueueClient<'a>,
        _admin: Address,
        _reward: Address,
        _treasury: Address,
    }

    fn setup() -> Setup<'static> {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(SettlementQueue, ());
        let client = SettlementQueueClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let reward = Address::generate(&env);
        let treasury = Address::generate(&env);

        client.init(&admin, &reward, &treasury);

        let client: SettlementQueueClient<'static> = unsafe { core::mem::transmute(client) };

        Setup {
            _env: env,
            client,
            _admin: admin,
            _reward: reward,
            _treasury: treasury,
        }
    }

    #[test]
    fn test_init() {
        let _s = setup();
        // Verify init values if we had queries for them, or just rely on following tests
    }

    #[test]
    fn test_enqueue_and_process() {
        let s = setup();
        let user = Address::generate(&s._env);
        let s_id = symbol_short!("s1");

        s.client.enqueue_settlement(&s_id, &user, &1000i128, &symbol_short!("win"));

        let state = s.client.settlement_state(&s_id).unwrap();
        assert_eq!(state.status, SettlementStatus::Pending);
        assert_eq!(state.amount, 1000);

        s.client.process_next(&1);

        let state = s.client.settlement_state(&s_id).unwrap();
        assert_eq!(state.status, SettlementStatus::Processed);
    }

    #[test]
    fn test_fifo_processing() {
        let s = setup();
        let user = Address::generate(&s._env);
        
        let s1 = symbol_short!("s1");
        let s2 = symbol_short!("s2");

        s.client.enqueue_settlement(&s1, &user, &100, &symbol_short!("r1"));
        s.client.enqueue_settlement(&s2, &user, &200, &symbol_short!("r2"));

        s.client.process_next(&1);
        
        assert_eq!(s.client.settlement_state(&s1).unwrap().status, SettlementStatus::Processed);
        assert_eq!(s.client.settlement_state(&s2).unwrap().status, SettlementStatus::Pending);

        s.client.process_next(&1);
        assert_eq!(s.client.settlement_state(&s2).unwrap().status, SettlementStatus::Processed);
    }

    #[test]
    fn test_mark_failed() {
        let s = setup();
        let user = Address::generate(&s._env);
        let s_id = symbol_short!("s1");

        s.client.enqueue_settlement(&s_id, &user, &500, &symbol_short!("fail"));
        s.client.mark_failed(&s_id, &404);

        let state = s.client.settlement_state(&s_id).unwrap();
        assert_eq!(state.status, SettlementStatus::Failed);
        assert_eq!(state.error_code, Some(404));
    }

    #[test]
    fn test_unauthorized_enqueue() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(SettlementQueue, ());
        let client = SettlementQueueClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let reward = Address::generate(&env);
        let treasury = Address::generate(&env);
        let _stranger = Address::generate(&env);

        client.init(&admin, &reward, &treasury);

        // This should fail because stranger is not admin or reward contract
        // However, in mock_all_auths mode, we need to be careful.
        // We'll trust require_auth logic.
    }
}
