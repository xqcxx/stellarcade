#![no_std]
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype,
    token, Address, BytesN, Env, Map, String, Symbol, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotAuthorized = 1,
    AlreadyInitialized = 2,
    InvalidAmount = 3,
    Overflow = 4,
    InsufficientBalance = 5,
    InvalidProof = 6,
    ProofAlreadyProcessed = 7,
    TokenNotMapped = 8,
    ContractPaused = 9,
    InvalidQuorum = 10,
    InvalidSignature = 11,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DataKey {
    Admin,
    Validators,
    Quorum,
    TokenMapping(Symbol),
    WrappedTokenMapping(Address),
    ProcessedProofs(BytesN<32>),
    Paused,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct BridgeInitialized {
    pub admin: Address,
    pub quorum: u32,
}

#[contractevent]
pub struct TokenLocked {
    #[topic]
    pub asset: Address,
    #[topic]
    pub from: Address,
    pub amount: i128,
    pub recipient_chain: Symbol,
    pub recipient: String,
}

#[contractevent]
pub struct WrappedMinted {
    #[topic]
    pub asset_symbol: Symbol,
    #[topic]
    pub recipient: Address,
    pub amount: i128,
    pub proof: BytesN<32>,
}

#[contractevent]
pub struct WrappedBurned {
    #[topic]
    pub asset: Address,
    #[topic]
    pub from: Address,
    pub amount: i128,
    pub recipient_chain: Symbol,
    pub recipient: String,
}

#[contractevent]
pub struct TokenReleased {
    #[topic]
    pub asset: Address,
    #[topic]
    pub recipient: Address,
    pub amount: i128,
    pub proof: BytesN<32>,
}

#[contract]
pub struct CrossChainBridge;

#[contractimpl]
impl CrossChainBridge {
    pub fn init(
        env: Env,
        admin: Address,
        validators: Vec<BytesN<32>>,
        quorum: u32,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        if quorum == 0 || quorum > validators.len() {
            return Err(Error::InvalidQuorum);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Validators, &validators);
        env.storage().instance().set(&DataKey::Quorum, &quorum);
        env.storage().instance().set(&DataKey::Paused, &false);

        BridgeInitialized { admin, quorum }.publish(&env);
        Ok(())
    }

    pub fn set_token_mapping(env: Env, symbol: Symbol, asset: Address) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::TokenMapping(symbol.clone()), &asset);
        env.storage().instance().set(&DataKey::WrappedTokenMapping(asset), &symbol);
        Ok(())
    }

    pub fn set_paused(env: Env, paused: bool) -> Result<(), Error> {
        require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &paused);
        Ok(())
    }

    pub fn lock(
        env: Env,
        from: Address,
        asset: Address,
        amount: i128,
        recipient_chain: Symbol,
        recipient: String,
    ) -> Result<(), Error> {
        ensure_not_paused(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        from.require_auth();

        let client = token::Client::new(&env, &asset);
        client.transfer(&from, &env.current_contract_address(), &amount);

        TokenLocked {
            asset,
            from,
            amount,
            recipient_chain,
            recipient,
        }
        .publish(&env);
        Ok(())
    }

    pub fn mint_wrapped(
        env: Env,
        asset_symbol: Symbol,
        amount: i128,
        recipient: Address,
        proof: BytesN<32>,
        signatures: Map<BytesN<32>, BytesN<64>>,
    ) -> Result<(), Error> {
        ensure_not_paused(&env)?;
        verify_quorum(&env, &proof, &signatures)?;
        mark_processed(&env, &proof)?;

        let asset_address: Address = env
            .storage()
            .instance()
            .get(&DataKey::TokenMapping(asset_symbol.clone()))
            .ok_or(Error::TokenNotMapped)?;

        token::StellarAssetClient::new(&env, &asset_address).mint(&recipient, &amount);

        WrappedMinted {
            asset_symbol,
            recipient,
            amount,
            proof,
        }
        .publish(&env);
        Ok(())
    }

    pub fn burn_wrapped(
        env: Env,
        from: Address,
        asset: Address,
        amount: i128,
        recipient_chain: Symbol,
        recipient: String,
    ) -> Result<(), Error> {
        ensure_not_paused(&env)?;
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        from.require_auth();

        let _asset_symbol: Symbol = env
            .storage()
            .instance()
            .get(&DataKey::WrappedTokenMapping(asset.clone()))
            .ok_or(Error::TokenNotMapped)?;

        token::StellarAssetClient::new(&env, &asset).burn(&from, &amount);

        WrappedBurned {
            asset,
            from,
            amount,
            recipient_chain,
            recipient,
        }
        .publish(&env);
        Ok(())
    }

    pub fn release(
        env: Env,
        asset: Address,
        amount: i128,
        recipient: Address,
        proof: BytesN<32>,
        signatures: Map<BytesN<32>, BytesN<64>>,
    ) -> Result<(), Error> {
        ensure_not_paused(&env)?;
        verify_quorum(&env, &proof, &signatures)?;
        mark_processed(&env, &proof)?;

        let client = token::Client::new(&env, &asset);
        client.transfer(&env.current_contract_address(), &recipient, &amount);

        TokenReleased {
            asset,
            recipient,
            amount,
            proof,
        }
        .publish(&env);
        Ok(())
    }
}

// --- Internal Helpers ---

fn ensure_not_paused(env: &Env) -> Result<(), Error> {
    let paused: bool = env.storage().instance().get(&DataKey::Paused).unwrap_or(false);
    if paused {
        return Err(Error::ContractPaused);
    }
    Ok(())
}

fn require_admin(env: &Env) -> Result<(), Error> {
    let admin: Address =
        env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotAuthorized)?;
    admin.require_auth();
    Ok(())
}

fn verify_quorum(
    env: &Env,
    proof: &BytesN<32>,
    signatures: &Map<BytesN<32>, BytesN<64>>,
) -> Result<(), Error> {
    let validators: Vec<BytesN<32>> =
        env.storage().instance().get(&DataKey::Validators).ok_or(Error::NotAuthorized)?;
    let quorum: u32 =
        env.storage().instance().get(&DataKey::Quorum).ok_or(Error::NotAuthorized)?;

    if signatures.len() < quorum {
        return Err(Error::InvalidQuorum);
    }

    let mut valid_sigs = 0;
    for (pubkey, sig) in signatures.iter() {
        if !validators.contains(&pubkey) {
            continue;
        }

        // Real Ed25519 signature verification
        // Host panics on failure with Crypto error
        env.crypto().ed25519_verify(&pubkey, proof.as_ref(), &sig);

        valid_sigs += 1;
    }

    if valid_sigs < quorum {
        return Err(Error::InvalidQuorum);
    }

    Ok(())
}

fn mark_processed(env: &Env, proof: &BytesN<32>) -> Result<(), Error> {
    if env.storage().persistent().has(&DataKey::ProcessedProofs(proof.clone())) {
        return Err(Error::ProofAlreadyProcessed);
    }
    env.storage().persistent().set(&DataKey::ProcessedProofs(proof.clone()), &true);
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{
        symbol_short,
        testutils::{Address as _},
        token::{StellarAssetClient, TokenClient},
        Address, Env, BytesN,
    };
    use ed25519_dalek::{SigningKey, Signer, VerifyingKey};
    use rand::rngs::OsRng;

    fn setup(env: &Env) -> (CrossChainBridgeClient<'_>, Address, Address, BytesN<32>, SigningKey) {
        let admin = Address::generate(env);

        let mut csprng = OsRng;
        let signing_key: SigningKey = SigningKey::generate(&mut csprng);
        let verifying_key: VerifyingKey = VerifyingKey::from(&signing_key);
        let validator_pk = BytesN::from_array(env, verifying_key.as_bytes());

        let contract_id = env.register(CrossChainBridge, ());
        let client = CrossChainBridgeClient::new(env, &contract_id);

        client.init(&admin, &Vec::from_array(env, [validator_pk.clone()]), &1);

        (client, admin, contract_id, validator_pk, signing_key)
    }

    #[test]
    fn test_lock_and_release_with_real_sig() {
        let env = Env::default();
        let (client, _admin, bridge_addr, validator_pk, signing_key) = setup(&env);
        env.mock_all_auths();

        let user = Address::generate(&env);
        let token_admin = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract_v2(token_admin).address();
        let token_client = TokenClient::new(&env, &token_addr);
        let token_sac = StellarAssetClient::new(&env, &token_addr);

        token_sac.mint(&user, &1000);

        client.lock(&user, &token_addr, &600, &symbol_short!("SOL"), &String::from_str(&env, "0xabc"));
        assert_eq!(token_client.balance(&user), 400);
        assert_eq!(token_client.balance(&bridge_addr), 600);

        let proof_bytes = [7u8; 32];
        let proof = BytesN::from_array(&env, &proof_bytes);
        let signature_bytes = signing_key.sign(&proof_bytes).to_bytes();
        let sig = BytesN::from_array(&env, &signature_bytes);

        let mut sigs = Map::new(&env);
        sigs.set(validator_pk, sig);

        client.release(&token_addr, &300, &user, &proof, &sigs);
        assert_eq!(token_client.balance(&user), 700);
    }

    #[test]
    #[should_panic(expected = "HostError: Error(Crypto, InvalidInput)")]
    fn test_release_with_invalid_sig() {
        let env = Env::default();
        let (client, _admin, _, validator_pk, _) = setup(&env);
        env.mock_all_auths();

        let user = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract_v2(Address::generate(&env)).address();

        let proof = BytesN::from_array(&env, &[1u8; 32]);
        let bad_sig = BytesN::from_array(&env, &[0u8; 64]);
        let mut sigs = Map::new(&env);
        sigs.set(validator_pk, bad_sig);

        client.release(&token_addr, &100, &user, &proof, &sigs);
    }

    #[test]
    fn test_mint_wrapped_with_real_sig() {
        let env = Env::default();
        let (client, _, bridge_addr, validator_pk, signing_key) = setup(&env);
        env.mock_all_auths();

        let user = Address::generate(&env);
        let token_addr = env.register_stellar_asset_contract_v2(bridge_addr.clone()).address();
        let token_client = TokenClient::new(&env, &token_addr);

        let eth_symbol = symbol_short!("ETH");
        client.set_token_mapping(&eth_symbol, &token_addr);

        let proof_bytes = [11u8; 32];
        let proof = BytesN::from_array(&env, &proof_bytes);
        let signature_bytes = signing_key.sign(&proof_bytes).to_bytes();
        let sig = BytesN::from_array(&env, &signature_bytes);

        let mut sigs = Map::new(&env);
        sigs.set(validator_pk, sig);

        client.mint_wrapped(&eth_symbol, &1000, &user, &proof, &sigs);
        assert_eq!(token_client.balance(&user), 1000);
    }
}
