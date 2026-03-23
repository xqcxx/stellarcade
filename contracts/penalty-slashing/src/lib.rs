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
    Treasury,
    Violation(Symbol),  // violation code → PenaltyRule
    Penalty(u64),       // penalty_id → PenaltyRecord
    NextPenaltyId,
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PenaltyStatus {
    Applied,
    Appealed,
    Resolved,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PenaltyRule {
    pub code: Symbol,
    pub slash_amount: i128,
    pub description_hash: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PenaltyRecord {
    pub penalty_id: u64,
    pub account: Address,
    pub code: Symbol,
    pub slash_amount: i128,
    pub context_hash: Symbol,
    pub status: PenaltyStatus,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct ViolationDefined {
    #[topic]
    pub code: Symbol,
    pub slash_amount: i128,
}

#[contractevent]
pub struct PenaltyApplied {
    #[topic]
    pub penalty_id: u64,
    #[topic]
    pub account: Address,
    pub code: Symbol,
    pub slash_amount: i128,
}

#[contractevent]
pub struct PenaltyAppealed {
    #[topic]
    pub penalty_id: u64,
    pub account: Address,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct PenaltySlashing;

#[contractimpl]
impl PenaltySlashing {
    /// Initialize with admin and treasury contract/address holding slashed funds.
    pub fn init(env: Env, admin: Address, treasury_contract: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Treasury, &treasury_contract);
        env.storage().instance().set(&DataKey::NextPenaltyId, &0u64);
    }

    /// Define or update a violation rule. Admin-only.
    pub fn define_violation(
        env: Env,
        code: Symbol,
        penalty_rule: PenaltyRule,
    ) {
        Self::require_admin(&env);
        assert!(penalty_rule.slash_amount >= 0, "Slash amount must be non-negative");
        env.storage().persistent().set(&DataKey::Violation(code.clone()), &penalty_rule);

        ViolationDefined { code, slash_amount: penalty_rule.slash_amount }.publish(&env);
    }

    /// Apply a penalty to an account. Admin-only.
    /// Slashes tokens from `account` and transfers them to the treasury.
    pub fn apply_penalty(
        env: Env,
        account: Address,
        code: Symbol,
        context_hash: Symbol,
        token_address: Address,
    ) -> u64 {
        Self::require_admin(&env);

        let rule: PenaltyRule = env
            .storage()
            .persistent()
            .get(&DataKey::Violation(code.clone()))
            .expect("Violation code not defined");

        let penalty_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextPenaltyId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextPenaltyId, &penalty_id.checked_add(1).expect("Overflow"));

        // Transfer slash amount from account to treasury
        if rule.slash_amount > 0 {
            let treasury: Address =
                env.storage().instance().get(&DataKey::Treasury).expect("Not initialized");
            let token_client = token::Client::new(&env, &token_address);
            token_client.transfer(&account, &treasury, &rule.slash_amount);
        }

        let record = PenaltyRecord {
            penalty_id,
            account: account.clone(),
            code: code.clone(),
            slash_amount: rule.slash_amount,
            context_hash,
            status: PenaltyStatus::Applied,
        };
        env.storage().persistent().set(&DataKey::Penalty(penalty_id), &record);

        PenaltyApplied { penalty_id, account, code, slash_amount: rule.slash_amount }.publish(&env);

        penalty_id
    }

    /// File an appeal for a penalty. Only the penalized account may appeal.
    pub fn appeal_penalty(env: Env, penalty_id: u64) {
        let mut record: PenaltyRecord = env
            .storage()
            .persistent()
            .get(&DataKey::Penalty(penalty_id))
            .expect("Penalty not found");

        record.account.require_auth();

        assert!(
            record.status == PenaltyStatus::Applied,
            "Can only appeal an applied penalty"
        );

        record.status = PenaltyStatus::Appealed;
        env.storage().persistent().set(&DataKey::Penalty(penalty_id), &record);

        PenaltyAppealed { penalty_id, account: record.account }.publish(&env);
    }

    /// Read current state of a penalty record.
    pub fn penalty_state(env: Env, penalty_id: u64) -> PenaltyRecord {
        env.storage()
            .persistent()
            .get(&DataKey::Penalty(penalty_id))
            .expect("Penalty not found")
    }

    // ── Internal helpers ──────────────────────────────────────────
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
        testutils::Address as _,
        token::{Client as TokenClient, StellarAssetClient},
        Env, Symbol,
    };

    fn setup_token<'a>(env: &Env, admin: &Address) -> (Address, StellarAssetClient<'a>, TokenClient<'a>) {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let addr = sac.address();
        (addr.clone(), StellarAssetClient::new(env, &addr), TokenClient::new(env, &addr))
    }

    #[test]
    fn test_define_and_apply_penalty() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let offender = Address::generate(&env);
        let treasury = Address::generate(&env);

        let (token_id, sa, tc) = setup_token(&env, &admin);
        sa.mint(&offender, &1000);

        let contract_id = env.register_contract(None, PenaltySlashing);
        let client = PenaltySlashingClient::new(&env, &contract_id);

        client.init(&admin, &treasury);

        let rule = PenaltyRule {
            code: Symbol::new(&env, "CHEAT"),
            slash_amount: 100,
            description_hash: Symbol::new(&env, "DESC1"),
        };
        client.define_violation(&Symbol::new(&env, "CHEAT"), &rule);

        let pid = client.apply_penalty(
            &offender,
            &Symbol::new(&env, "CHEAT"),
            &Symbol::new(&env, "CTX1"),
            &token_id,
        );

        assert_eq!(tc.balance(&offender), 900);
        assert_eq!(tc.balance(&treasury), 100);

        let state = client.penalty_state(&pid);
        assert_eq!(state.status, PenaltyStatus::Applied);
    }

    #[test]
    fn test_appeal_penalty() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let offender = Address::generate(&env);
        let treasury = Address::generate(&env);

        let (token_id, sa, _) = setup_token(&env, &admin);
        sa.mint(&offender, &500);

        let contract_id = env.register_contract(None, PenaltySlashing);
        let client = PenaltySlashingClient::new(&env, &contract_id);

        client.init(&admin, &treasury);
        let rule = PenaltyRule {
            code: Symbol::new(&env, "AFK"),
            slash_amount: 50,
            description_hash: Symbol::new(&env, "DSAFK"),
        };
        client.define_violation(&Symbol::new(&env, "AFK"), &rule);

        let pid = client.apply_penalty(
            &offender,
            &Symbol::new(&env, "AFK"),
            &Symbol::new(&env, "CTX2"),
            &token_id,
        );

        client.appeal_penalty(&pid);
        let state = client.penalty_state(&pid);
        assert_eq!(state.status, PenaltyStatus::Appealed);
    }

    #[test]
    #[should_panic(expected = "Violation code not defined")]
    fn test_apply_undefined_violation_fails() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();
        let admin = Address::generate(&env);
        let offender = Address::generate(&env);
        let treasury = Address::generate(&env);
        let token = Address::generate(&env);

        let contract_id = env.register_contract(None, PenaltySlashing);
        let client = PenaltySlashingClient::new(&env, &contract_id);
        client.init(&admin, &treasury);
        client.apply_penalty(&offender, &Symbol::new(&env, "BOGUS"), &Symbol::new(&env, "C"), &token);
    }
}
