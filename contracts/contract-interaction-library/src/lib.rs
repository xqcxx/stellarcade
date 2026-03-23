#![no_std]

//! # Contract Interaction Library
//!
//! A reusable on-chain SDK helper exposing type-safe wrappers, cross-contract
//! call utilities, and event-decoding helpers so that other StellarCade
//! contracts can interact with the ecosystem in a composable, safe way.
//!
//! **Capabilities:**
//! 1. **Registry** – store and resolve canonical contract addresses by name,
//!    with version tracking and activation state.
//! 2. **Upgrade management** – update a registered contract's address while
//!    preserving name-based routing.
//! 3. **Call logging** – emit and persist immutable records of cross-contract
//!    call outcomes for auditability.

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype, Address, Env, Map, String,
};

// ─── Types ────────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContractEntry {
    pub name: String,
    pub address: Address,
    pub version: u32,
    pub active: bool,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallRecord {
    pub callee_name: String,
    pub caller: Address,
    pub timestamp: u64,
    pub success: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Registry,
    CallCounter,
    CallLog,
}

// ─── Events ───────────────────────────────────────────────────────────────────

#[contractevent]
pub struct LibraryInitialized {
    pub admin: Address,
}

#[contractevent]
pub struct ContractRegistered {
    pub name: String,
    pub address: Address,
    pub version: u32,
}

#[contractevent]
pub struct ContractDeactivated {
    pub name: String,
}

#[contractevent]
pub struct CallLogged {
    #[topic]
    pub log_id: u64,
    pub callee_name: String,
    pub caller: Address,
    pub success: bool,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct ContractInteractionLibrary;

#[contractimpl]
impl ContractInteractionLibrary {
    /// Initialise the library contract. Must be called once.
    pub fn init(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        let empty: Map<String, ContractEntry> = Map::new(&env);
        env.storage().instance().set(&DataKey::Registry, &empty);
        let empty_log: Map<u64, CallRecord> = Map::new(&env);
        env.storage().instance().set(&DataKey::CallLog, &empty_log);
        env.storage().instance().set(&DataKey::CallCounter, &0u64);
        LibraryInitialized { admin }.publish(&env);
    }

    // ── Registry ──────────────────────────────────────────────────────────────

    /// Register a contract under a human-readable `name` (1-32 chars, unique).
    pub fn register_contract(env: Env, name: String, address: Address, version: u32) {
        Self::require_admin(&env);
        if name.len() == 0 || name.len() > 32 {
            panic!("Invalid name: must be 1-32 characters");
        }
        if version == 0 {
            panic!("Invalid version: must be positive");
        }
        let mut registry: Map<String, ContractEntry> =
            env.storage().instance().get(&DataKey::Registry).unwrap_or(Map::new(&env));
        if registry.contains_key(name.clone()) {
            panic!("Contract name already registered");
        }
        let entry = ContractEntry {
            name: name.clone(),
            address: address.clone(),
            version,
            active: true,
        };
        registry.set(name.clone(), entry);
        env.storage().instance().set(&DataKey::Registry, &registry);
        ContractRegistered { name, address, version }.publish(&env);
    }

    /// Deactivate a registered contract by name.
    pub fn deactivate_contract(env: Env, name: String) {
        Self::require_admin(&env);
        let mut registry: Map<String, ContractEntry> =
            env.storage().instance().get(&DataKey::Registry).unwrap_or(Map::new(&env));
        let mut entry = registry.get(name.clone()).expect("Contract not found");
        entry.active = false;
        registry.set(name.clone(), entry);
        env.storage().instance().set(&DataKey::Registry, &registry);
        ContractDeactivated { name }.publish(&env);
    }

    /// Upgrade a registered contract to a new address + version.
    pub fn upgrade_contract(env: Env, name: String, new_address: Address, new_version: u32) {
        Self::require_admin(&env);
        if new_version == 0 {
            panic!("Invalid version: must be positive");
        }
        let mut registry: Map<String, ContractEntry> =
            env.storage().instance().get(&DataKey::Registry).unwrap_or(Map::new(&env));
        let mut entry = registry.get(name.clone()).expect("Contract not found");
        entry.address = new_address.clone();
        entry.version = new_version;
        entry.active = true;
        registry.set(name.clone(), entry);
        env.storage().instance().set(&DataKey::Registry, &registry);
    }

    // ── Lookup ────────────────────────────────────────────────────────────────

    /// Return the full registry entry for `name`.
    pub fn get_contract(env: Env, name: String) -> ContractEntry {
        let registry: Map<String, ContractEntry> =
            env.storage().instance().get(&DataKey::Registry).unwrap_or(Map::new(&env));
        registry.get(name).expect("Contract not found")
    }

    /// Resolve the address of an active registered contract.
    pub fn resolve(env: Env, name: String) -> Address {
        let entry = Self::get_contract(env.clone(), name);
        if !entry.active {
            panic!("Contract is inactive");
        }
        entry.address
    }

    // ── Call Logging ──────────────────────────────────────────────────────────

    /// Record a cross-contract call result and return its log ID.
    pub fn log_call(env: Env, callee_name: String, caller: Address, success: bool) -> u64 {
        caller.require_auth();
        let record = CallRecord {
            callee_name: callee_name.clone(),
            caller: caller.clone(),
            timestamp: env.ledger().timestamp(),
            success,
        };
        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::CallCounter)
            .unwrap_or(0);
        let mut log: Map<u64, CallRecord> = env
            .storage()
            .instance()
            .get(&DataKey::CallLog)
            .unwrap_or(Map::new(&env));
        log.set(id, record);
        env.storage().instance().set(&DataKey::CallLog, &log);
        env.storage().instance().set(&DataKey::CallCounter, &(id + 1));
        CallLogged {
            log_id: id,
            callee_name,
            caller,
            success,
        }
        .publish(&env);
        id
    }

    /// Fetch a call log entry by ID.
    pub fn get_call_log(env: Env, log_id: u64) -> CallRecord {
        let log: Map<u64, CallRecord> = env
            .storage()
            .instance()
            .get(&DataKey::CallLog)
            .unwrap_or(Map::new(&env));
        log.get(log_id).expect("Log entry not found")
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn require_admin(env: &Env) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("Not initialized");
        admin.require_auth();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Env};

    fn setup() -> (Env, ContractInteractionLibraryClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register(ContractInteractionLibrary, ());
        let client = ContractInteractionLibraryClient::new(&env, &contract_id);
        client.init(&admin);
        (env, client, admin)
    }

    #[test]
    fn test_init() {
        let _ = setup();
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let (_, client, admin) = setup();
        client.init(&admin);
    }

    #[test]
    fn test_register_and_resolve() {
        let (env, client, _admin) = setup();
        let target = Address::generate(&env);
        let name = String::from_str(&env, "token-contract");
        client.register_contract(&name, &target, &1);
        let resolved = client.resolve(&name);
        assert_eq!(resolved, target);
    }

    #[test]
    #[should_panic(expected = "Contract name already registered")]
    fn test_duplicate_name_rejected() {
        let (env, client, _admin) = setup();
        let addr = Address::generate(&env);
        let name = String::from_str(&env, "foo");
        client.register_contract(&name, &addr, &1);
        client.register_contract(&name, &addr, &2);
    }

    #[test]
    #[should_panic(expected = "Contract is inactive")]
    fn test_deactivate_blocks_resolve() {
        let (env, client, _admin) = setup();
        let addr = Address::generate(&env);
        let name = String::from_str(&env, "bar");
        client.register_contract(&name, &addr, &1);
        client.deactivate_contract(&name);
        client.resolve(&name);
    }

    #[test]
    fn test_upgrade_reactivates() {
        let (env, client, _admin) = setup();
        let addr = Address::generate(&env);
        let addr2 = Address::generate(&env);
        let name = String::from_str(&env, "baz");
        client.register_contract(&name, &addr, &1);
        client.deactivate_contract(&name);
        client.upgrade_contract(&name, &addr2, &2);
        assert_eq!(client.resolve(&name), addr2);
    }

    #[test]
    #[should_panic(expected = "Invalid name")]
    fn test_empty_name_rejected() {
        let (env, client, _) = setup();
        let addr = Address::generate(&env);
        client.register_contract(&String::from_str(&env, ""), &addr, &1);
    }

    #[test]
    #[should_panic(expected = "Invalid version")]
    fn test_zero_version_rejected() {
        let (env, client, _) = setup();
        let addr = Address::generate(&env);
        client.register_contract(&String::from_str(&env, "valid"), &addr, &0);
    }

    #[test]
    #[should_panic(expected = "Contract not found")]
    fn test_unknown_contract_panics() {
        let (env, client, _) = setup();
        client.get_contract(&String::from_str(&env, "ghost"));
    }

    #[test]
    fn test_call_log_roundtrip() {
        let (env, client, _) = setup();
        let caller = Address::generate(&env);
        let callee = String::from_str(&env, "staking");
        let id = client.log_call(&callee, &caller, &true);
        let record = client.get_call_log(&id);
        assert!(record.success);
        assert_eq!(record.callee_name, callee);
    }

    #[test]
    fn test_call_log_increments() {
        let (env, client, _) = setup();
        let caller = Address::generate(&env);
        let callee = String::from_str(&env, "game");
        let id0 = client.log_call(&callee, &caller, &true);
        let id1 = client.log_call(&callee, &caller, &false);
        assert_eq!(id0, 0);
        assert_eq!(id1, 1);
    }

    #[test]
    fn test_get_contract_returns_full_entry() {
        let (env, client, _) = setup();
        let addr = Address::generate(&env);
        let name = String::from_str(&env, "my-contract");
        client.register_contract(&name, &addr, &3);
        let entry = client.get_contract(&name);
        assert_eq!(entry.version, 3);
        assert!(entry.active);
    }
}
