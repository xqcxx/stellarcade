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
    Token,
    Escrow(u64), // escrow_id → EscrowState
    NextId,
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EscrowStatus {
    Active,
    Released,
    Cancelled,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EscrowState {
    pub escrow_id: u64,
    pub payer: Address,
    pub payee: Address,
    pub amount: i128,
    pub terms_hash: Symbol,
    pub status: EscrowStatus,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct EscrowCreated {
    #[topic]
    pub escrow_id: u64,
    pub payer: Address,
    pub payee: Address,
    pub amount: i128,
    pub terms_hash: Symbol,
}

#[contractevent]
pub struct EscrowReleased {
    #[topic]
    pub escrow_id: u64,
    pub payee: Address,
    pub amount: i128,
}

#[contractevent]
pub struct EscrowCancelled {
    #[topic]
    pub escrow_id: u64,
    pub payer: Address,
    pub amount: i128,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct EscrowVault;

#[contractimpl]
impl EscrowVault {
    /// Initialize with the admin and the accepted token address.
    pub fn init(env: Env, admin: Address, token_address: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Token, &token_address);
        env.storage().instance().set(&DataKey::NextId, &0u64);
    }

    /// Create a new escrow. The payer locks `amount` tokens into the contract.
    pub fn create_escrow(
        env: Env,
        payer: Address,
        payee: Address,
        amount: i128,
        terms_hash: Symbol,
    ) -> u64 {
        assert!(amount > 0, "Amount must be positive");
        payer.require_auth();

        // Transfer tokens from payer to this contract
        let token_addr: Address =
            env.storage().instance().get(&DataKey::Token).expect("Not initialized");
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(&payer, &env.current_contract_address(), &amount);

        // Assign ID
        let escrow_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextId, &(escrow_id.checked_add(1).expect("Overflow")));

        let state = EscrowState {
            escrow_id,
            payer: payer.clone(),
            payee: payee.clone(),
            amount,
            terms_hash: terms_hash.clone(),
            status: EscrowStatus::Active,
        };
        env.storage().persistent().set(&DataKey::Escrow(escrow_id), &state);

        EscrowCreated { escrow_id, payer, payee, amount, terms_hash }.publish(&env);

        escrow_id
    }

    /// Release escrow funds to the payee. Only the admin or payer may release.
    pub fn release_escrow(env: Env, caller: Address, escrow_id: u64) {
        caller.require_auth();

        let mut state: EscrowState = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .expect("Escrow not found");

        assert!(
            state.status == EscrowStatus::Active,
            "Escrow is not active"
        );

        let admin: Address =
            env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        assert!(
            caller == admin || caller == state.payer,
            "Unauthorized: must be admin or payer"
        );

        state.status = EscrowStatus::Released;
        env.storage().persistent().set(&DataKey::Escrow(escrow_id), &state);

        // Transfer to payee
        let token_addr: Address =
            env.storage().instance().get(&DataKey::Token).expect("Not initialized");
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(
            &env.current_contract_address(),
            &state.payee,
            &state.amount,
        );

        EscrowReleased { escrow_id, payee: state.payee, amount: state.amount }.publish(&env);
    }

    /// Cancel an active escrow and return funds to the payer. Admin-only.
    pub fn cancel_escrow(env: Env, escrow_id: u64) {
        let admin: Address =
            env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        admin.require_auth();

        let mut state: EscrowState = env
            .storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .expect("Escrow not found");

        assert!(
            state.status == EscrowStatus::Active,
            "Escrow is not active"
        );

        state.status = EscrowStatus::Cancelled;
        env.storage().persistent().set(&DataKey::Escrow(escrow_id), &state);

        let token_addr: Address =
            env.storage().instance().get(&DataKey::Token).expect("Not initialized");
        let token_client = token::Client::new(&env, &token_addr);
        token_client.transfer(
            &env.current_contract_address(),
            &state.payer,
            &state.amount,
        );

        EscrowCancelled { escrow_id, payer: state.payer, amount: state.amount }.publish(&env);
    }

    /// Read the state of an escrow.
    pub fn escrow_state(env: Env, escrow_id: u64) -> EscrowState {
        env.storage()
            .persistent()
            .get(&DataKey::Escrow(escrow_id))
            .expect("Escrow not found")
    }
}

// ── Tests ─────────────────────────────────────────────────────────
#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _},
        token::{Client as TokenClient, StellarAssetClient},
        Env,
    };

    fn create_token<'a>(env: &Env, admin: &Address) -> (Address, StellarAssetClient<'a>, TokenClient<'a>) {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        let addr = sac.address();
        let sa_client = StellarAssetClient::new(env, &addr);
        let t_client = TokenClient::new(env, &addr);
        (addr, sa_client, t_client)
    }

    #[test]
    fn test_create_and_release() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let payer = Address::generate(&env);
        let payee = Address::generate(&env);

        let (token_id, sa_client, token_client) = create_token(&env, &admin);
        sa_client.mint(&payer, &1000);

        let contract_id = env.register_contract(None, EscrowVault);
        let client = EscrowVaultClient::new(&env, &contract_id);

        client.init(&admin, &token_id);
        let id = client.create_escrow(&payer, &payee, &500, &symbol_short!("HASH1"));

        assert_eq!(token_client.balance(&contract_id), 500);
        assert_eq!(token_client.balance(&payer), 500);

        let state = client.escrow_state(&id);
        assert_eq!(state.status, EscrowStatus::Active);

        client.release_escrow(&payer, &id);
        assert_eq!(token_client.balance(&payee), 500);

        let state = client.escrow_state(&id);
        assert_eq!(state.status, EscrowStatus::Released);
    }

    #[test]
    fn test_cancel_returns_funds() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let payer = Address::generate(&env);
        let payee = Address::generate(&env);

        let (token_id, sa_client, token_client) = create_token(&env, &admin);
        sa_client.mint(&payer, &1000);

        let contract_id = env.register_contract(None, EscrowVault);
        let client = EscrowVaultClient::new(&env, &contract_id);

        client.init(&admin, &token_id);
        let id = client.create_escrow(&payer, &payee, &300, &symbol_short!("HASH2"));

        client.cancel_escrow(&id);
        assert_eq!(token_client.balance(&payer), 1000);

        let state = client.escrow_state(&id);
        assert_eq!(state.status, EscrowStatus::Cancelled);
    }

    #[test]
    #[should_panic(expected = "Escrow is not active")]
    fn test_double_release_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let payer = Address::generate(&env);
        let payee = Address::generate(&env);

        let (token_id, sa_client, _) = create_token(&env, &admin);
        sa_client.mint(&payer, &1000);

        let contract_id = env.register_contract(None, EscrowVault);
        let client = EscrowVaultClient::new(&env, &contract_id);

        client.init(&admin, &token_id);
        let id = client.create_escrow(&payer, &payee, &100, &symbol_short!("HASH3"));
        client.release_escrow(&payer, &id);
        // Should panic
        client.release_escrow(&payer, &id);
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let token = Address::generate(&env);
        let contract_id = env.register_contract(None, EscrowVault);
        let client = EscrowVaultClient::new(&env, &contract_id);
        client.init(&admin, &token);
        client.init(&admin, &token);
    }
}
