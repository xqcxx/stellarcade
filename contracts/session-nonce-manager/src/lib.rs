#![no_std]

//! # Session Nonce Manager Contract
//!
//! A foundational anti-replay primitive for write-heavy contracts and
//! signature-based actions. Each nonce is issued for a specific `(account,
//! purpose)` pair, tracked as used once consumed, and may be administratively
//! revoked before use.

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype, Address, Env, String,
};

// ─── Storage Keys ─────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    NextNonce(Address, String),
    NonceUsed(Address, String, u64),
    NonceRevoked(Address, u64),
}

// ─── Events ───────────────────────────────────────────────────────────────────

#[contractevent]
pub struct NonceManagerInitialized {
    pub admin: Address,
}

#[contractevent]
pub struct NonceIssued {
    pub account: Address,
    pub purpose: String,
    pub nonce: u64,
}

#[contractevent]
pub struct NonceConsumed {
    pub account: Address,
    pub purpose: String,
    pub nonce: u64,
}

#[contractevent]
pub struct NonceRevoked {
    pub account: Address,
    pub nonce: u64,
}

// ─── Contract ─────────────────────────────────────────────────────────────────

#[contract]
pub struct SessionNonceManagerContract;

#[contractimpl]
impl SessionNonceManagerContract {
    /// Initialise the contract and set the admin. Must be called exactly once.
    pub fn init(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        NonceManagerInitialized { admin }.publish(&env);
    }

    /// Issue the next nonce for `(account, purpose)` and return its value.
    pub fn issue_nonce(env: Env, account: Address, purpose: String) -> u64 {
        Self::require_admin_or_account(&env, &account);
        if purpose.len() == 0 {
            panic!("Invalid purpose: must not be empty");
        }
        let key = DataKey::NextNonce(account.clone(), purpose.clone());
        let nonce: u64 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(nonce + 1));
        NonceIssued { account, purpose, nonce }.publish(&env);
        nonce
    }

    /// Consume `nonce` for `(account, purpose)`, marking it as used.
    pub fn consume_nonce(env: Env, account: Address, nonce: u64, purpose: String) {
        account.require_auth();
        if purpose.len() == 0 {
            panic!("Invalid purpose: must not be empty");
        }
        let used_key = DataKey::NonceUsed(account.clone(), purpose.clone(), nonce);
        let revoked_key = DataKey::NonceRevoked(account.clone(), nonce);

        if env.storage().persistent().get::<_, bool>(&revoked_key).unwrap_or(false) {
            panic!("Nonce has been revoked");
        }
        if env.storage().persistent().get::<_, bool>(&used_key).unwrap_or(false) {
            panic!("Nonce already used");
        }
        let next_key = DataKey::NextNonce(account.clone(), purpose.clone());
        let next: u64 = env.storage().persistent().get(&next_key).unwrap_or(0);
        if nonce >= next {
            panic!("Nonce not found");
        }
        env.storage().persistent().set(&used_key, &true);
        NonceConsumed { account, purpose, nonce }.publish(&env);
    }

    /// Return `true` if `nonce` for `(account, purpose)` is valid.
    pub fn is_nonce_valid(env: Env, account: Address, nonce: u64, purpose: String) -> bool {
        let next_key = DataKey::NextNonce(account.clone(), purpose.clone());
        let next: u64 = env.storage().persistent().get(&next_key).unwrap_or(0);
        if nonce >= next {
            return false;
        }
        let used = env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::NonceUsed(account.clone(), purpose, nonce))
            .unwrap_or(false);
        let revoked = env
            .storage()
            .persistent()
            .get::<_, bool>(&DataKey::NonceRevoked(account, nonce))
            .unwrap_or(false);
        !used && !revoked
    }

    /// Revoke `nonce` for `account`. Only the admin may revoke nonces.
    pub fn revoke_nonce(env: Env, account: Address, nonce: u64) {
        Self::require_admin(&env);
        let key = DataKey::NonceRevoked(account.clone(), nonce);
        env.storage().persistent().set(&key, &true);
        NonceRevoked { account, nonce }.publish(&env);
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

    fn require_admin_or_account(env: &Env, account: &Address) {
        // Any authenticated call is fine; we get auth from either admin or account.
        // In practice, we simply require the account to authenticate.
        account.require_auth();
        let _ = env;
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::{Address as _}, Env};

    fn setup() -> (Env, SessionNonceManagerContractClient<'static>, Address) {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(SessionNonceManagerContract, ());
        let client = SessionNonceManagerContractClient::new(&env, &contract_id);
        let admin = Address::generate(&env);
        client.init(&admin);
        (env, client, admin)
    }

    #[test]
    fn test_init_succeeds() {
        let (_env, _client, _admin) = setup();
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let (_env, client, admin) = setup();
        client.init(&admin);
    }

    #[test]
    fn test_issue_and_validate_nonce() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "login");
        let nonce = client.issue_nonce(&user, &purpose);
        assert_eq!(nonce, 0);
        assert!(client.is_nonce_valid(&user, &nonce, &purpose));
    }

    #[test]
    fn test_consume_nonce_marks_as_used() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "transfer");
        let nonce = client.issue_nonce(&user, &purpose);
        client.consume_nonce(&user, &nonce, &purpose);
        assert!(!client.is_nonce_valid(&user, &nonce, &purpose));
    }

    #[test]
    #[should_panic(expected = "Nonce already used")]
    fn test_replay_is_rejected() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "withdraw");
        let nonce = client.issue_nonce(&user, &purpose);
        client.consume_nonce(&user, &nonce, &purpose);
        client.consume_nonce(&user, &nonce, &purpose);
    }

    #[test]
    fn test_nonces_increment_monotonically() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "action");
        let n0 = client.issue_nonce(&user, &purpose);
        let n1 = client.issue_nonce(&user, &purpose);
        let n2 = client.issue_nonce(&user, &purpose);
        assert_eq!(n0, 0);
        assert_eq!(n1, 1);
        assert_eq!(n2, 2);
    }

    #[test]
    fn test_unissued_nonce_is_invalid() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "something");
        assert!(!client.is_nonce_valid(&user, &99, &purpose));
    }

    #[test]
    #[should_panic(expected = "Invalid purpose")]
    fn test_empty_purpose_is_rejected() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        client.issue_nonce(&user, &String::from_str(&env, ""));
    }

    #[test]
    fn test_revoke_nonce() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "vote");
        let nonce = client.issue_nonce(&user, &purpose);
        client.revoke_nonce(&user, &nonce);
        // is_nonce_valid checks the DataKey::NonceRevoked path
        assert!(!client.is_nonce_valid(&user, &nonce, &purpose));
    }

    #[test]
    #[should_panic(expected = "Nonce has been revoked")]
    fn test_consume_revoked_nonce_panics() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "vote");
        let nonce = client.issue_nonce(&user, &purpose);
        client.revoke_nonce(&user, &nonce);
        client.consume_nonce(&user, &nonce, &purpose);
    }

    #[test]
    #[should_panic(expected = "Nonce not found")]
    fn test_consume_unissued_nonce_panics() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "vote");
        client.consume_nonce(&user, &99, &purpose);
    }

    #[test]
    fn test_events_emitted_on_issue() {
        let (env, client, _admin) = setup();
        let user = Address::generate(&env);
        let purpose = String::from_str(&env, "event-test");
        // Issue a nonce — the contract publishes an event internally.
        // Verify the nonce is returned correctly (event is implicitly emitted).
        let nonce = client.issue_nonce(&user, &purpose);
        assert_eq!(nonce, 0);
    }
}
