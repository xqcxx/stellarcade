#![no_std]

use soroban_sdk::{
    contract, contractevent, contractimpl, contracttype,
    token, Address, Env, Symbol, Vec,
};

// ── Storage Keys ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    Token,
    SplitConfig(Symbol),                   // stream_id → SplitConfig
    StreamBalance(Symbol),                  // stream_id → i128 (total deposited, not yet distributed)
    RecipientBalance(Symbol, Address),      // (stream_id, recipient) → i128
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecipientWeight {
    pub recipient: Address,
    /// Weight in basis points (0–10000). All recipients must sum to 10000.
    pub weight_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SplitConfig {
    pub stream_id: Symbol,
    pub recipients: Vec<RecipientWeight>,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct SplitConfigured {
    #[topic]
    pub stream_id: Symbol,
}

#[contractevent]
pub struct RevenueDeposited {
    #[topic]
    pub stream_id: Symbol,
    pub amount: i128,
}

#[contractevent]
pub struct RevenueDistributed {
    #[topic]
    pub stream_id: Symbol,
    pub total: i128,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct RevenueSplit;

#[contractimpl]
impl RevenueSplit {
    /// Initialize with admin and the token used for splits.
    pub fn init(env: Env, admin: Address, token_address: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token_address);
    }

    /// Configure or update a split for a stream. Admin-only.
    /// Recipient weights must sum to exactly 10000 BPS.
    pub fn set_split_config(env: Env, stream_id: Symbol, recipients: Vec<RecipientWeight>) {
        Self::require_admin(&env);
        assert!(!recipients.is_empty(), "Recipients cannot be empty");

        let mut total_bps: u32 = 0;
        for r in recipients.iter() {
            total_bps = total_bps
                .checked_add(r.weight_bps)
                .expect("Overflow in weight sum");
        }
        assert!(total_bps == 10_000, "Weights must sum to 10000 BPS");

        let config = SplitConfig {
            stream_id: stream_id.clone(),
            recipients,
        };
        env.storage().persistent().set(&DataKey::SplitConfig(stream_id.clone()), &config);

        SplitConfigured { stream_id }.publish(&env);
    }

    /// Deposit revenue into a stream. Any caller may deposit; they must auth.
    pub fn deposit_revenue(env: Env, depositor: Address, stream_id: Symbol, amount: i128) {
        assert!(amount > 0, "Amount must be positive");
        depositor.require_auth();

        // Ensure config exists
        assert!(
            env.storage().persistent().has(&DataKey::SplitConfig(stream_id.clone())),
            "Split config not found for stream"
        );

        let token_addr: Address =
            env.storage().instance().get(&DataKey::Token).expect("Not initialized");
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&depositor, &env.current_contract_address(), &amount);

        let current: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::StreamBalance(stream_id.clone()))
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&DataKey::StreamBalance(stream_id.clone()), &(current.checked_add(amount).expect("Overflow")));

        RevenueDeposited { stream_id, amount }.publish(&env);
    }

    /// Distribute all pending revenue in a stream to recipients. Admin-only.
    pub fn distribute(env: Env, stream_id: Symbol) {
        Self::require_admin(&env);

        let config: SplitConfig = env
            .storage()
            .persistent()
            .get(&DataKey::SplitConfig(stream_id.clone()))
            .expect("Split config not found");

        let total: i128 = env
            .storage()
            .persistent()
            .get(&DataKey::StreamBalance(stream_id.clone()))
            .unwrap_or(0);

        assert!(total > 0, "Nothing to distribute");

        // Zero out the stream balance before transfers (reentrancy guard)
        env.storage()
            .persistent()
            .set(&DataKey::StreamBalance(stream_id.clone()), &0i128);

        let token_addr: Address =
            env.storage().instance().get(&DataKey::Token).expect("Not initialized");
        let token_client = token::Client::new(&env, &token_addr);

        for r in config.recipients.iter() {
            let share = total
                .checked_mul(r.weight_bps as i128)
                .expect("Overflow")
                .checked_div(10_000)
                .expect("Division by zero");

            if share > 0 {
                // Credit to recipient internal balance
                let bal_key = DataKey::RecipientBalance(stream_id.clone(), r.recipient.clone());
                let prev: i128 = env.storage().persistent().get(&bal_key).unwrap_or(0);
                env.storage()
                    .persistent()
                    .set(&bal_key, &prev.checked_add(share).expect("Overflow"));

                // Immediate transfer
                token_client.transfer(&env.current_contract_address(), &r.recipient, &share);
            }
        }

        RevenueDistributed { stream_id, total }.publish(&env);
    }

    /// Query cumulative amount distributed to a recipient for a stream.
    pub fn recipient_balance(env: Env, stream_id: Symbol, recipient: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::RecipientBalance(stream_id, recipient))
            .unwrap_or(0)
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
        testutils::Address as _,
        token::{Client as TokenClient, StellarAssetClient},
        vec, Env, Symbol,
    };

    fn setup_token<'a>(env: &Env, admin: &Address) -> (Address, StellarAssetClient<'a>, TokenClient<'a>) {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let addr = sac.address();
        (addr.clone(), StellarAssetClient::new(env, &addr), TokenClient::new(env, &addr))
    }

    #[test]
    fn test_configure_deposit_distribute() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let r1 = Address::generate(&env);
        let r2 = Address::generate(&env);
        let depositor = Address::generate(&env);

        let (token_id, sa, tc) = setup_token(&env, &admin);
        sa.mint(&depositor, &1000);

        let contract_id = env.register_contract(None, RevenueSplit);
        let client = RevenueSplitClient::new(&env, &contract_id);

        client.init(&admin, &token_id);

        let stream = Symbol::new(&env, "gaming");
        let recipients = vec![
            &env,
            RecipientWeight { recipient: r1.clone(), weight_bps: 6000 },
            RecipientWeight { recipient: r2.clone(), weight_bps: 4000 },
        ];
        client.set_split_config(&stream, &recipients);

        client.deposit_revenue(&depositor, &stream, &1000);
        assert_eq!(tc.balance(&contract_id), 1000);

        client.distribute(&stream);
        assert_eq!(tc.balance(&r1), 600);
        assert_eq!(tc.balance(&r2), 400);

        assert_eq!(client.recipient_balance(&stream, &r1), 600);
        assert_eq!(client.recipient_balance(&stream, &r2), 400);
    }

    #[test]
    #[should_panic(expected = "Weights must sum to 10000 BPS")]
    fn test_invalid_weight_sum_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let r1 = Address::generate(&env);
        let token = Address::generate(&env);

        let contract_id = env.register_contract(None, RevenueSplit);
        let client = RevenueSplitClient::new(&env, &contract_id);
        client.init(&admin, &token);

        let stream = Symbol::new(&env, "bad");
        let recipients = vec![
            &env,
            RecipientWeight { recipient: r1, weight_bps: 5000 }, // Only 50%, not 100%
        ];
        client.set_split_config(&stream, &recipients);
    }

    #[test]
    #[should_panic(expected = "Nothing to distribute")]
    fn test_distribute_empty_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let r1 = Address::generate(&env);
        let token = Address::generate(&env);

        let contract_id = env.register_contract(None, RevenueSplit);
        let client = RevenueSplitClient::new(&env, &contract_id);
        client.init(&admin, &token);

        let stream = Symbol::new(&env, "empty");
        let recipients = vec![&env, RecipientWeight { recipient: r1, weight_bps: 10000 }];
        client.set_split_config(&stream, &recipients);
        client.distribute(&stream);
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let contract_id = env.register_contract(None, RevenueSplit);
        let client = RevenueSplitClient::new(&env, &contract_id);
        client.init(&admin, &token);
        client.init(&admin, &token);
    }
}
