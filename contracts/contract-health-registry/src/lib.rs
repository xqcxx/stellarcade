#![no_std]

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype,
    Address, Env, Symbol, Vec,
};

// ── Storage Keys ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    HealthPolicy(Address),  // contract_id → HealthPolicy
    LatestHealth(Address),  // contract_id → HealthReport
    HealthHistory(Address), // contract_id → Vec<HealthReport>
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Critical,
    Unknown,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthReport {
    pub contract_id: Address,
    pub status: HealthStatus,
    /// Arbitrary details/metadata hash reported by the oracle/monitor.
    pub details_hash: Symbol,
    pub timestamp: u64,
    pub reported_by: Address,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthPolicy {
    pub contract_id: Address,
    /// Symbol describing the policy (e.g., "strict", "lenient").
    pub policy_type: Symbol,
    /// Max number of history entries to retain.
    pub max_history: u32,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct HealthReported {
    pub contract_id: Address,
    pub status: HealthStatus,
    pub timestamp: u64,
}

#[contractevent]
pub struct PolicySet {
    pub contract_id: Address,
    pub policy_type: Symbol,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct ContractHealthRegistry;

#[contractimpl]
impl ContractHealthRegistry {
    /// Initialize with the admin address.
    pub fn init(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    /// Report the health of a contract. The reporter must be authorized.
    /// Admin can report for any contract; other monitors must be pre-approved (future extension).
    pub fn report_health(
        env: Env,
        reporter: Address,
        contract_id: Address,
        status: HealthStatus,
        details_hash: Symbol,
    ) {
        reporter.require_auth();

        let admin: Address = env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        // Only admin may report in this version; circuit-breaker roles can extend this later
        assert!(reporter == admin, "Unauthorized reporter");

        let report = HealthReport {
            contract_id: contract_id.clone(),
            status: status.clone(),
            details_hash,
            timestamp: env.ledger().timestamp(),
            reported_by: reporter,
        };

        // Update latest report
        env.storage()
            .persistent()
            .set(&DataKey::LatestHealth(contract_id.clone()), &report);

        // Append to history, respecting max_history
        let policy: Option<HealthPolicy> = env
            .storage()
            .persistent()
            .get(&DataKey::HealthPolicy(contract_id.clone()));

        let max_history = policy.as_ref().map(|p| p.max_history).unwrap_or(10);

        let mut history: Vec<HealthReport> = env
            .storage()
            .persistent()
            .get(&DataKey::HealthHistory(contract_id.clone()))
            .unwrap_or(Vec::new(&env));

        history.push_back(report.clone());

        // Trim to max_history
        while history.len() > max_history {
            history.remove(0);
        }

        env.storage()
            .persistent()
            .set(&DataKey::HealthHistory(contract_id.clone()), &history);

        HealthReported {
            contract_id,
            status,
            timestamp: report.timestamp,
        }
        .publish(&env);
    }

    /// Set the health monitoring policy for a contract. Admin-only.
    pub fn set_health_policy(env: Env, contract_id: Address, policy: HealthPolicy) {
        Self::require_admin(&env);

        assert!(policy.max_history > 0, "max_history must be at least 1");

        env.storage()
            .persistent()
            .set(&DataKey::HealthPolicy(contract_id.clone()), &policy);

        PolicySet { contract_id, policy_type: policy.policy_type }.publish(&env);
    }

    /// Get the most recent health report for a contract.
    pub fn health_of(env: Env, contract_id: Address) -> HealthReport {
        env.storage()
            .persistent()
            .get(&DataKey::LatestHealth(contract_id))
            .expect("No health data for contract")
    }

    /// Get the full health history for a contract (up to max_history entries).
    pub fn history(env: Env, contract_id: Address) -> Vec<HealthReport> {
        env.storage()
            .persistent()
            .get(&DataKey::HealthHistory(contract_id))
            .unwrap_or(Vec::new(&env))
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
    use soroban_sdk::{testutils::Address as _, Env, Symbol};

    #[test]
    fn test_report_and_query_health() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let monitored = Address::generate(&env);

        let contract_id = env.register_contract(None, ContractHealthRegistry);
        let client = ContractHealthRegistryClient::new(&env, &contract_id);

        client.init(&admin);

        client.report_health(
            &admin,
            &monitored,
            &HealthStatus::Healthy,
            &Symbol::new(&env, "OK1"),
        );

        let report = client.health_of(&monitored);
        assert_eq!(report.status, HealthStatus::Healthy);
        assert_eq!(report.contract_id, monitored);
    }

    #[test]
    fn test_history_accumulates() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let monitored = Address::generate(&env);

        let contract_id = env.register_contract(None, ContractHealthRegistry);
        let client = ContractHealthRegistryClient::new(&env, &contract_id);

        client.init(&admin);

        client.report_health(&admin, &monitored, &HealthStatus::Healthy, &Symbol::new(&env, "H1"));
        client.report_health(&admin, &monitored, &HealthStatus::Degraded, &Symbol::new(&env, "H2"));
        client.report_health(&admin, &monitored, &HealthStatus::Critical, &Symbol::new(&env, "H3"));

        let hist = client.history(&monitored);
        assert_eq!(hist.len(), 3);
        assert_eq!(hist.get(2).unwrap().status, HealthStatus::Critical);
    }

    #[test]
    fn test_history_trimmed_by_policy() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let monitored = Address::generate(&env);

        let contract_id = env.register_contract(None, ContractHealthRegistry);
        let client = ContractHealthRegistryClient::new(&env, &contract_id);

        client.init(&admin);

        let policy = HealthPolicy {
            contract_id: monitored.clone(),
            policy_type: Symbol::new(&env, "strict"),
            max_history: 2,
        };
        client.set_health_policy(&monitored, &policy);

        client.report_health(&admin, &monitored, &HealthStatus::Healthy, &Symbol::new(&env, "A"));
        client.report_health(&admin, &monitored, &HealthStatus::Degraded, &Symbol::new(&env, "B"));
        client.report_health(&admin, &monitored, &HealthStatus::Critical, &Symbol::new(&env, "C"));

        let hist = client.history(&monitored);
        // Only 2 most recent
        assert_eq!(hist.len(), 2);
        assert_eq!(hist.get(0).unwrap().status, HealthStatus::Degraded);
        assert_eq!(hist.get(1).unwrap().status, HealthStatus::Critical);
    }

    #[test]
    #[should_panic(expected = "Unauthorized reporter")]
    fn test_unauthorized_reporter_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let bad_actor = Address::generate(&env);
        let monitored = Address::generate(&env);

        let contract_id = env.register_contract(None, ContractHealthRegistry);
        let client = ContractHealthRegistryClient::new(&env, &contract_id);

        client.init(&admin);
        client.report_health(&bad_actor, &monitored, &HealthStatus::Healthy, &Symbol::new(&env, "X"));
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register_contract(None, ContractHealthRegistry);
        let client = ContractHealthRegistryClient::new(&env, &contract_id);
        client.init(&admin);
        client.init(&admin);
    }
}
