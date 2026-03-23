#![no_std]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Env, String,
};

// ---------------------------------------------------------------------------
// TTL constants
// ---------------------------------------------------------------------------

/// ~30 days at 5 s / ledger
const PERSISTENT_BUMP_LEDGERS: u32 = 518_400;
/// Threshold at which a persistent entry is renewed (~7 days from expiry)
const PERSISTENT_BUMP_THRESHOLD: u32 = PERSISTENT_BUMP_LEDGERS - 100_800;

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
    CampaignNotFound = 4,
    CampaignAlreadyExists = 5,
    CampaignExhausted = 6,
    CampaignNotActive = 7,
    NothingToClaim = 8,
    AlreadyClaimed = 9,
    InvalidAmount = 10,
    Overflow = 11,
}

// ---------------------------------------------------------------------------
// Storage Keys
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    NftContract,
    RewardContract,
    /// CampaignData keyed by campaign_id
    Campaign(u32),
    /// Pending claim flag keyed by (user, campaign_id)
    PendingReward(Address, u32),
    /// Claimed status keyed by (user, campaign_id)
    Claimed(Address, u32),
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CampaignData {
    pub metadata_uri: String,
    pub supply: u32,
    pub remaining: u32,
    pub is_active: bool,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[contractevent]
pub struct CampaignDefined {
    #[topic]
    pub campaign_id: u32,
    pub metadata_uri: String,
    pub supply: u32,
}

#[contractevent]
pub struct RewardMinted {
    #[topic]
    pub campaign_id: u32,
    #[topic]
    pub user: Address,
}

#[contractevent]
pub struct RewardClaimed {
    #[topic]
    pub campaign_id: u32,
    #[topic]
    pub user: Address,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct NftReward;

#[contractimpl]
impl NftReward {
    /// Initialize the contract.
    pub fn init(
        env: Env,
        admin: Address,
        nft_contract: Address,
        reward_contract: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NftContract, &nft_contract);
        env.storage()
            .instance()
            .set(&DataKey::RewardContract, &reward_contract);

        Ok(())
    }

    /// Define a new NFT reward campaign. Admin only.
    pub fn define_nft_reward(
        env: Env,
        campaign_id: u32,
        metadata_uri: String,
        supply: u32,
    ) -> Result<(), Error> {
        let admin = Self::require_initialized(&env)?;
        admin.require_auth();

        if supply == 0 {
            return Err(Error::InvalidAmount);
        }

        let key = DataKey::Campaign(campaign_id);
        if env.storage().persistent().has(&key) {
            return Err(Error::CampaignAlreadyExists);
        }

        let campaign = CampaignData {
            metadata_uri: metadata_uri.clone(),
            supply,
            remaining: supply,
            is_active: true,
        };

        env.storage().persistent().set(&key, &campaign);
        env.storage().persistent().extend_ttl(
            &key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        CampaignDefined { campaign_id, metadata_uri, supply }.publish(&env);

        Ok(())
    }

    /// Mark a reward as "minted" (awarded) for a user. Admin only.
    pub fn mint_reward(env: Env, user: Address, campaign_id: u32) -> Result<(), Error> {
        let admin = Self::require_initialized(&env)?;
        admin.require_auth();

        let campaign_key = DataKey::Campaign(campaign_id);
        let mut campaign: CampaignData = env
            .storage()
            .persistent()
            .get(&campaign_key)
            .ok_or(Error::CampaignNotFound)?;

        if campaign.remaining == 0 {
            return Err(Error::CampaignExhausted);
        }

        if !campaign.is_active {
            return Err(Error::CampaignNotActive);
        }

        let pending_key = DataKey::PendingReward(user.clone(), campaign_id);
        if env.storage().persistent().has(&pending_key) {
            return Err(Error::AlreadyClaimed); // Or already minted
        }

        // Decrement supply
        campaign.remaining -= 1;
        if campaign.remaining == 0 {
            campaign.is_active = false;
        }

        env.storage().persistent().set(&campaign_key, &campaign);
        env.storage().persistent().extend_ttl(
            &campaign_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // Mark as pending
        env.storage().persistent().set(&pending_key, &true);
        env.storage().persistent().extend_ttl(
            &pending_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        RewardMinted { campaign_id, user }.publish(&env);

        Ok(())
    }

    /// Claim the pending NFT reward. User only.
    pub fn claim_nft(env: Env, user: Address, campaign_id: u32) -> Result<(), Error> {
        Self::require_initialized(&env)?;
        user.require_auth();

        let claimed_key = DataKey::Claimed(user.clone(), campaign_id);
        if env.storage().persistent().has(&claimed_key) {
            return Err(Error::AlreadyClaimed);
        }

        let pending_key = DataKey::PendingReward(user.clone(), campaign_id);
        if !env.storage().persistent().has(&pending_key) {
            return Err(Error::NothingToClaim);
        }

        // Set claimed before external call (Reentrancy guard)
        env.storage().persistent().set(&claimed_key, &true);
        env.storage().persistent().extend_ttl(
            &claimed_key,
            PERSISTENT_BUMP_THRESHOLD,
            PERSISTENT_BUMP_LEDGERS,
        );

        // Remove pending
        env.storage().persistent().remove(&pending_key);

        RewardClaimed { campaign_id, user }.publish(&env);

        Ok(())
    }

    /// View campaign state.
    pub fn nft_reward_state(env: Env, campaign_id: u32) -> Option<CampaignData> {
        env.storage().persistent().get(&DataKey::Campaign(campaign_id))
    }

    // -----------------------------------------------------------------------
    // Internal Helpers
    // -----------------------------------------------------------------------

    fn require_initialized(env: &Env) -> Result<Address, Error> {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, Env, String};

    struct Setup {
        env: Env,
        client: NftRewardClient<'static>,
        admin: Address,
        nft: Address,
        reward: Address,
    }

    fn setup() -> Setup {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(NftReward, ());
        let client = NftRewardClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let reward = Address::generate(&env);

        client.init(&admin, &nft, &reward);

        // SAFETY: client borrows env by reference
        let client: NftRewardClient<'static> = unsafe { core::mem::transmute(client) };

        Setup {
            env,
            client,
            admin,
            nft,
            reward,
        }
    }

    #[test]
    fn test_init_succeeds() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register(NftReward, ());
        let client = NftRewardClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let nft = Address::generate(&env);
        let reward = Address::generate(&env);

        client.init(&admin, &nft, &reward);
    }

    #[test]
    fn test_init_twice_fails() {
        let s = setup();
        let result = s.client.try_init(&s.admin, &s.nft, &s.reward);
        assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
    }

    #[test]
    fn test_define_reward_succeeds() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &100u32);

        let state = s.client.nft_reward_state(&1u32).unwrap();
        assert_eq!(state.supply, 100);
        assert_eq!(state.remaining, 100);
        assert_eq!(state.metadata_uri, uri);
        assert!(state.is_active);
    }

    #[test]
    fn test_mint_reward_succeeds() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &10u32);

        let user = Address::generate(&s.env);
        s.client.mint_reward(&user, &1u32);

        let state = s.client.nft_reward_state(&1u32).unwrap();
        assert_eq!(state.remaining, 9);
    }

    #[test]
    fn test_claim_nft_succeeds() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &10u32);

        let user = Address::generate(&s.env);
        s.client.mint_reward(&user, &1u32);

        s.client.claim_nft(&user, &1u32);
    }

    #[test]
    fn test_claim_without_mint_fails() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &10u32);

        let user = Address::generate(&s.env);
        let result = s.client.try_claim_nft(&user, &1u32);
        assert_eq!(result, Err(Ok(Error::NothingToClaim)));
    }

    #[test]
    fn test_double_claim_fails() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &10u32);

        let user = Address::generate(&s.env);
        s.client.mint_reward(&user, &1u32);
        s.client.claim_nft(&user, &1u32);

        let result = s.client.try_claim_nft(&user, &1u32);
        assert_eq!(result, Err(Ok(Error::AlreadyClaimed)));
    }

    #[test]
    fn test_exhausted_supply() {
        let s = setup();
        let uri = String::from_str(&s.env, "ipfs://test");
        s.client.define_nft_reward(&1u32, &uri, &1u32);

        let user1 = Address::generate(&s.env);
        let user2 = Address::generate(&s.env);

        s.client.mint_reward(&user1, &1u32);
        let result = s.client.try_mint_reward(&user2, &1u32);
        assert_eq!(result, Err(Ok(Error::CampaignExhausted)));
    }
}
