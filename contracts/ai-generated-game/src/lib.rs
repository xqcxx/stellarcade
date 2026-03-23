#![no_std]
use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, BytesN, Env,
    String,
};

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum DataKey {
    Admin,
    ModelOracle,
    RewardContract,
    // Maps a game ID to the game state
    Game(u64),
    // Maps (game_id, player_address) to boolean indicator of reward status
    Reward(u64, Address),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub enum GameStatus {
    Created,
    InProgress,
    Resolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[contracttype]
pub struct AIGameState {
    pub config_hash: BytesN<32>,
    pub status: GameStatus,
    pub winner: Option<Address>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[contracterror]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    GameAlreadyExists = 4,
    GameNotFound = 5,
    InvalidStatus = 6,
    RewardAlreadyClaimed = 7,
    NoReward = 8,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct ContractInitialized {
    pub admin: Address,
    pub model_oracle: Address,
    pub reward_contract: Address,
}

#[contractevent]
pub struct GameCreated {
    #[topic]
    pub game_id: u64,
    pub config_hash: BytesN<32>,
}

#[contractevent]
pub struct MovePlayed {
    #[topic]
    pub game_id: u64,
    #[topic]
    pub player: Address,
    pub move_payload: String,
}

#[contractevent]
pub struct GameResolved {
    #[topic]
    pub game_id: u64,
    #[topic]
    pub oracle: Address,
    pub result_payload: String,
    pub winner: Option<Address>,
}

#[contractevent]
pub struct RewardClaimed {
    #[topic]
    pub game_id: u64,
    #[topic]
    pub player: Address,
}

#[contract]
pub struct AIGeneratedGameContract;

#[contractimpl]
impl AIGeneratedGameContract {
    /// Initialize the contract with the admin, AI model oracle address, and reward system address.
    pub fn init(
        env: Env,
        admin: Address,
        model_oracle: Address,
        reward_contract: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }

        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::ModelOracle, &model_oracle);
        env.storage().instance().set(&DataKey::RewardContract, &reward_contract);

        ContractInitialized {
            admin: admin.clone(),
            model_oracle,
            reward_contract,
        }
        .publish(&env);
        Ok(())
    }

    /// Setup a new AI-generated game layout.
    pub fn create_ai_game(
        env: Env,
        admin: Address,
        game_id: u64,
        config_hash: BytesN<32>,
    ) -> Result<(), Error> {
        admin.require_auth();
        let stored_admin: Address =
            env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotInitialized)?;

        if admin != stored_admin {
            return Err(Error::Unauthorized);
        }

        let game_key = DataKey::Game(game_id);
        if env.storage().persistent().has(&game_key) {
            return Err(Error::GameAlreadyExists);
        }

        let state = AIGameState {
            config_hash: config_hash.clone(),
            status: GameStatus::Created,
            winner: None,
        };

        env.storage().persistent().set(&game_key, &state);
        GameCreated { game_id, config_hash }.publish(&env);
        Ok(())
    }

    /// Player submitting a move towards an active AI game.
    pub fn submit_ai_move(
        env: Env,
        player: Address,
        game_id: u64,
        move_payload: String,
    ) -> Result<(), Error> {
        player.require_auth();

        let game_key = DataKey::Game(game_id);
        let mut state: AIGameState =
            env.storage().persistent().get(&game_key).ok_or(Error::GameNotFound)?;

        if state.status == GameStatus::Created {
            state.status = GameStatus::InProgress;
            env.storage().persistent().set(&game_key, &state);
        } else if state.status != GameStatus::InProgress {
            return Err(Error::InvalidStatus);
        }

        MovePlayed {
            game_id,
            player,
            move_payload,
        }
        .publish(&env);
        Ok(())
    }

    /// Oracle node resolves the game securely mapping outputs and winners systematically.
    pub fn resolve_ai_game(
        env: Env,
        oracle: Address,
        game_id: u64,
        result_payload: String,
        winner: Option<Address>,
    ) -> Result<(), Error> {
        oracle.require_auth();
        let stored_oracle: Address = env
            .storage()
            .instance()
            .get(&DataKey::ModelOracle)
            .ok_or(Error::NotInitialized)?;

        if oracle != stored_oracle {
            return Err(Error::Unauthorized);
        }

        let game_key = DataKey::Game(game_id);
        let mut state: AIGameState =
            env.storage().persistent().get(&game_key).ok_or(Error::GameNotFound)?;

        if state.status == GameStatus::Resolved {
            return Err(Error::InvalidStatus);
        }

        state.status = GameStatus::Resolved;
        state.winner = winner.clone();

        env.storage().persistent().set(&game_key, &state);

        if let Some(w) = winner.clone() {
            env.storage().persistent().set(&DataKey::Reward(game_id, w.clone()), &true);
        }

        GameResolved {
            game_id,
            oracle: oracle.clone(),
            result_payload,
            winner,
        }
        .publish(&env);
        Ok(())
    }

    /// Authorizes player to claim rewards mapped after oracle validation finishes.
    pub fn claim_ai_reward(env: Env, player: Address, game_id: u64) -> Result<(), Error> {
        player.require_auth();

        let game_key = DataKey::Game(game_id);
        let state: AIGameState =
            env.storage().persistent().get(&game_key).ok_or(Error::GameNotFound)?;

        if state.status != GameStatus::Resolved {
            return Err(Error::InvalidStatus);
        }

        let reward_key = DataKey::Reward(game_id, player.clone());
        let can_claim_opt: Option<bool> = env.storage().persistent().get(&reward_key);

        if can_claim_opt.is_none() {
            return Err(Error::NoReward);
        }

        let can_claim = can_claim_opt.unwrap();
        if !can_claim {
            return Err(Error::RewardAlreadyClaimed);
        }

        env.storage().persistent().set(&reward_key, &false);

        // Ensure reward tracking was allocated correctly globally securely via event binding
        RewardClaimed {
            game_id,
            player: player.clone(),
        }
        .publish(&env);

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Address, BytesN, Env, String};

    #[test]
    fn test_initialization() {
        let env = Env::default();
        let contract_id = env.register(AIGeneratedGameContract, ());
        let client = AIGeneratedGameContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let reward_system = Address::generate(&env);

        client.init(&admin, &oracle, &reward_system);

        let init_result = client.try_init(&admin, &oracle, &reward_system);
        assert_eq!(init_result, Err(Ok(Error::AlreadyInitialized)));
    }

    #[test]
    fn test_game_flow() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(AIGeneratedGameContract, ());
        let client = AIGeneratedGameContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let reward_system = Address::generate(&env);
        let player = Address::generate(&env);

        client.init(&admin, &oracle, &reward_system);

        let game_id: u64 = 1;
        let config_hash = BytesN::from_array(&env, &[0; 32]);

        client.create_ai_game(&admin, &game_id, &config_hash);

        // Assert dup creation rejection natively
        let dup_create = client.try_create_ai_game(&admin, &game_id, &config_hash);
        assert_eq!(dup_create, Err(Ok(Error::GameAlreadyExists)));

        let move_payload = String::from_str(&env, "player1_move");
        client.submit_ai_move(&player, &game_id, &move_payload);

        let result_payload = String::from_str(&env, "score: 100");
        client.resolve_ai_game(&oracle, &game_id, &result_payload, &Some(player.clone()));

        // Cannot claim rewards of someone else
        let player2 = Address::generate(&env);
        let reward_fail = client.try_claim_ai_reward(&player2, &game_id);
        assert_eq!(reward_fail, Err(Ok(Error::NoReward)));

        // Successful claim mapping logic transitions correctly
        client.claim_ai_reward(&player, &game_id);

        // Cannot reclaim
        let double_claim = client.try_claim_ai_reward(&player, &game_id);
        assert_eq!(double_claim, Err(Ok(Error::RewardAlreadyClaimed)));
    }
}
