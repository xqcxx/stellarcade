#![no_std]

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype, Address, Env, Symbol,
};

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Role(Symbol, Address),
}

// Predefined roles as constants for reuse
use soroban_sdk::symbol_short;
pub const ADMIN: Symbol = symbol_short!("ADMIN");
pub const OPERATOR: Symbol = symbol_short!("OPERATOR");
pub const PAUSER: Symbol = symbol_short!("PAUSER");
pub const GAME: Symbol = symbol_short!("GAME");

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct RoleGranted {
    pub role: Symbol,
    pub account: Address,
}

#[contractevent]
pub struct RoleRevoked {
    pub role: Symbol,
    pub account: Address,
}

#[contract]
pub struct AccessControl;

#[contractimpl]
impl AccessControl {
    /// Initializes the contract with a super admin.
    /// This admin will have the power to grant and revoke any roles.
    pub fn init(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);

        // The admin also gets the ADMIN role by default for internal consistency
        internal_grant_role(&env, ADMIN, admin);
    }

    /// Grants a role to an account. Only accounts with ADMIN role (or the super admin) can call this.
    pub fn grant_role(env: Env, role: Symbol, account: Address) {
        require_admin(&env);
        internal_grant_role(&env, role, account);
    }

    /// Revokes a role from an account. Only accounts with ADMIN role can call this.
    pub fn revoke_role(env: Env, role: Symbol, account: Address) {
        require_admin(&env);
        internal_revoke_role(&env, role, account);
    }

    /// Checks if an account has a specific role.
    pub fn has_role(env: Env, role: Symbol, account: Address) -> bool {
        internal_has_role(&env, role, account)
    }

    /// Retrieves the current super admin address.
    pub fn get_admin(env: Env) -> Address {
        env.storage().instance().get(&DataKey::Admin).expect("Not initialized")
    }
}

// --- Library Functions / Guard Helpers ---
// These functions are designed to be used either matching the contract logic
// or as a shared module for other contracts.

pub fn require_admin(env: &Env) {
    let admin: Address = env.storage()
        .instance()
        .get(&DataKey::Admin)
        .expect("AccessControl: Not initialized");
    admin.require_auth();
}

pub fn require_role(env: &Env, role: Symbol, account: Address) {
    if !internal_has_role(env, role, account) {
        panic!("AccessControl: Missing required role");
    }
}

pub fn internal_grant_role(env: &Env, role: Symbol, account: Address) {
    let key = DataKey::Role(role.clone(), account.clone());
    if !env.storage().persistent().has(&key) {
        env.storage().persistent().set(&key, &());

        // Emit role change event
        RoleGranted { role, account }.publish(env);
    }
}

pub fn internal_revoke_role(env: &Env, role: Symbol, account: Address) {
    let key = DataKey::Role(role.clone(), account.clone());
    if env.storage().persistent().has(&key) {
        env.storage().persistent().remove(&key);

        // Emit role change event
        RoleRevoked { role, account }.publish(env);
    }
}

pub fn internal_has_role(env: &Env, role: Symbol, account: Address) -> bool {
    env.storage().persistent().has(&DataKey::Role(role, account))
}

#[cfg(test)]
mod test;
