#![no_std]

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype,
    Address, Env, Symbol,
};

// ── Storage Keys ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    MinDelay,
    Upgrade(u64),       // upgrade_id → UpgradeRecord
    NextUpgradeId,
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpgradeStatus {
    Queued,
    Executed,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpgradeRecord {
    pub upgrade_id: u64,
    pub target_contract: Address,
    pub payload_hash: Symbol,
    /// Earliest timestamp (in seconds) at which execute_upgrade may be called.
    pub eta: u64,
    pub status: UpgradeStatus,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct UpgradeQueued {
    pub upgrade_id: u64,
    pub target_contract: Address,
    pub eta: u64,
}

#[contractevent]
pub struct UpgradeCancelled {
    pub upgrade_id: u64,
}

#[contractevent]
pub struct UpgradeExecuted {
    pub upgrade_id: u64,
    pub target_contract: Address,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct ContractUpgradeTimelock;

#[contractimpl]
impl ContractUpgradeTimelock {
    /// Initialize with admin and minimum timelock delay (seconds).
    pub fn init(env: Env, admin: Address, min_delay: u64) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::MinDelay, &min_delay);
        env.storage().instance().set(&DataKey::NextUpgradeId, &0u64);
    }

    /// Queue an upgrade proposal. Admin-only.
    /// `eta` must be at least `now + min_delay`.
    pub fn queue_upgrade(
        env: Env,
        target_contract: Address,
        payload_hash: Symbol,
        eta: u64,
    ) -> u64 {
        Self::require_admin(&env);

        let now = env.ledger().timestamp();
        let min_delay: u64 = env
            .storage()
            .instance()
            .get(&DataKey::MinDelay)
            .unwrap_or(0);

        assert!(
            eta >= now.checked_add(min_delay).expect("Overflow"),
            "ETA too soon: must respect minimum delay"
        );

        let upgrade_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextUpgradeId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextUpgradeId, &upgrade_id.checked_add(1).expect("Overflow"));

        let record = UpgradeRecord {
            upgrade_id,
            target_contract: target_contract.clone(),
            payload_hash,
            eta,
            status: UpgradeStatus::Queued,
        };
        env.storage().persistent().set(&DataKey::Upgrade(upgrade_id), &record);

        UpgradeQueued { upgrade_id, target_contract, eta }.publish(&env);

        upgrade_id
    }

    /// Cancel a queued upgrade. Admin-only.
    pub fn cancel_upgrade(env: Env, upgrade_id: u64) {
        Self::require_admin(&env);

        let mut record: UpgradeRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Upgrade(upgrade_id))
            .expect("Upgrade not found");

        assert!(
            record.status == UpgradeStatus::Queued,
            "Upgrade is not in Queued state"
        );

        record.status = UpgradeStatus::Cancelled;
        env.storage().persistent().set(&DataKey::Upgrade(upgrade_id), &record);

        UpgradeCancelled { upgrade_id }.publish(&env);
    }

    /// Execute a queued upgrade after the timelock has elapsed. Admin-only.
    pub fn execute_upgrade(env: Env, upgrade_id: u64) {
        Self::require_admin(&env);

        let mut record: UpgradeRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Upgrade(upgrade_id))
            .expect("Upgrade not found");

        assert!(
            record.status == UpgradeStatus::Queued,
            "Upgrade is not in Queued state"
        );

        let now = env.ledger().timestamp();
        assert!(now >= record.eta, "Timelock has not elapsed");

        record.status = UpgradeStatus::Executed;
        env.storage().persistent().set(&DataKey::Upgrade(upgrade_id), &record);

        UpgradeExecuted { upgrade_id, target_contract: record.target_contract }.publish(&env);
    }

    /// Read the state of an upgrade record.
    pub fn upgrade_state(env: Env, upgrade_id: u64) -> UpgradeRecord {
        env.storage()
            .persistent()
            .get(&DataKey::Upgrade(upgrade_id))
            .expect("Upgrade not found")
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
        Env, Symbol,
    };

    fn set_time(env: &Env, ts: u64) {
        env.ledger().set(LedgerInfo {
            timestamp: ts,
            protocol_version: 25,
            sequence_number: ts as u32,
            network_id: [0u8; 32],
            base_reserve: 0,
            min_temp_entry_ttl: 0,
            min_persistent_entry_ttl: 0,
            max_entry_ttl: 1_000_000,
        });
    }

    #[test]
    fn test_queue_and_execute() {
        let env = Env::default();
        env.mock_all_auths();

        set_time(&env, 1000);

        let admin = Address::generate(&env);
        let target = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractUpgradeTimelock);
        let client = ContractUpgradeTimelockClient::new(&env, &contract_id);

        client.init(&admin, &86400u64);  // 1 day delay

        let uid = client.queue_upgrade(
            &target,
            &Symbol::new(&env, "HASH1"),
            &(1000 + 86400 + 1),
        );

        // Advance past eta
        set_time(&env, 1000 + 86400 + 100);

        client.execute_upgrade(&uid);
        let state = client.upgrade_state(&uid);
        assert_eq!(state.status, UpgradeStatus::Executed);
    }

    #[test]
    #[should_panic(expected = "Timelock has not elapsed")]
    fn test_execute_too_early_fails() {
        let env = Env::default();
        env.mock_all_auths();

        set_time(&env, 1000);

        let admin = Address::generate(&env);
        let target = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractUpgradeTimelock);
        let client = ContractUpgradeTimelockClient::new(&env, &contract_id);

        client.init(&admin, &86400u64);
        let uid = client.queue_upgrade(
            &target,
            &Symbol::new(&env, "H2"),
            &(1000 + 86400 + 1),
        );

        // Do NOT advance time
        client.execute_upgrade(&uid);
    }

    #[test]
    fn test_cancel_upgrade() {
        let env = Env::default();
        env.mock_all_auths();

        set_time(&env, 1000);

        let admin = Address::generate(&env);
        let target = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractUpgradeTimelock);
        let client = ContractUpgradeTimelockClient::new(&env, &contract_id);

        client.init(&admin, &3600u64);
        let uid = client.queue_upgrade(
            &target,
            &Symbol::new(&env, "H3"),
            &(1000 + 3600 + 1),
        );

        client.cancel_upgrade(&uid);
        let state = client.upgrade_state(&uid);
        assert_eq!(state.status, UpgradeStatus::Cancelled);
    }

    #[test]
    #[should_panic(expected = "ETA too soon")]
    fn test_eta_too_soon_fails() {
        let env = Env::default();
        env.mock_all_auths();
        set_time(&env, 1000);

        let admin = Address::generate(&env);
        let target = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractUpgradeTimelock);
        let client = ContractUpgradeTimelockClient::new(&env, &contract_id);

        client.init(&admin, &86400u64);
        client.queue_upgrade(&target, &Symbol::new(&env, "H4"), &500u64);
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractUpgradeTimelock);
        let client = ContractUpgradeTimelockClient::new(&env, &contract_id);
        client.init(&admin, &0u64);
        client.init(&admin, &0u64);
    }
}
