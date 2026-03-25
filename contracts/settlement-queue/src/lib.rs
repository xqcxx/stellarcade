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
    Processing = 3,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchStatus {
    pub pending: u32,
    pub processing: u32,
    pub succeeded: u32,
    pub failed: u32,
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
                settlement.status = SettlementStatus::Processed;
                env.storage().persistent().set(&settlement_key, &settlement);
                
                SettlementProcessed { settlement_id: settlement_id.clone(), status: SettlementStatus::Processed }.publish(&env);
            }

            // Head always increments, effectively "popping" the queue
            head += 1;
            processed_count += 1;
            
            // NOTE: We no longer remove the ItemKey immediately to preserve 
            // the index -> settlement_id mapping for historical batch status queries.
            // In a production environment, would implement a separate TTL-based GC.
            env.storage().persistent().extend_ttl(
                &item_key,
                PERSISTENT_BUMP_THRESHOLD,
                PERSISTENT_BUMP_LEDGERS,
            );
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

    /// Query current batch status by index range.
    pub fn get_batch_status(env: Env, start_index: u64, end_index: u64) -> Result<BatchStatus, Error> {
        if start_index > end_index {
            return Err(Error::InvalidState); // Range error
        }

        let tail: u64 = env.storage().instance().get(&DataKey::QueueTail).unwrap_or(0);
        if end_index >= tail && tail > 0 {
             // We could cap or error; for validation, we'll error if start is out of bounds
             // but here we'll just check if start exists.
             if start_index >= tail {
                 return Ok(BatchStatus { pending: 0, processing: 0, succeeded: 0, failed: 0 });
             }
        }

        let mut status = BatchStatus {
            pending: 0,
            processing: 0,
            succeeded: 0,
            failed: 0,
        };

        // Safety cap for iteration
        let effective_end = if end_index < tail { end_index } else { tail.saturating_sub(1) };

        for index in start_index..=effective_end {
            let item_key = DataKey::QueueItem(index);
            if let Some(settlement_id) = env.storage().persistent().get::<_, Symbol>(&item_key) {
                let settlement_key = DataKey::Settlement(settlement_id);
                if let Some(settlement) = env.storage().persistent().get::<_, SettlementData>(&settlement_key) {
                    match settlement.status {
                        SettlementStatus::Pending => status.pending += 1,
                        SettlementStatus::Processed => status.succeeded += 1,
                        SettlementStatus::Failed => status.failed += 1,
                        SettlementStatus::Processing => status.processing += 1,
                    }
                }
            }
        }

        Ok(status)
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

#[cfg(test)]
mod test;
