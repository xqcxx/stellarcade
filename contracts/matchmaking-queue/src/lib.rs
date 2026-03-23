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
    QueueState(Symbol),  // queue_id → MatchQueueState
    NextMatchId,
    Match(u64),          // match_id → MatchRecord
}

// ── Domain Types ─────────────────────────────────────────────────
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchQueueState {
    pub queue_id: Symbol,
    pub players: Vec<Address>,
    pub criteria_hash: Symbol,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchRecord {
    pub match_id: u64,
    pub queue_id: Symbol,
    pub players: Vec<Address>,
}

// ── Events ────────────────────────────────────────────────────────
#[contractevent]
pub struct PlayerEnqueued {
    #[topic]
    pub queue_id: Symbol,
    pub player: Address,
}

#[contractevent]
pub struct PlayerDequeued {
    #[topic]
    pub queue_id: Symbol,
    pub player: Address,
}

#[contractevent]
pub struct MatchCreated {
    #[topic]
    pub match_id: u64,
    pub queue_id: Symbol,
}

// ── Contract ──────────────────────────────────────────────────────
#[contract]
pub struct MatchmakingQueue;

#[contractimpl]
impl MatchmakingQueue {
    /// Initialize the contract with an admin.
    pub fn init(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("Already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextMatchId, &0u64);
    }

    /// Enqueue a player into a matchmaking queue. Player must auth.
    pub fn enqueue_player(
        env: Env,
        queue_id: Symbol,
        player: Address,
        criteria_hash: Symbol,
    ) {
        player.require_auth();

        let mut state: MatchQueueState = env
            .storage()
            .persistent()
            .get(&DataKey::QueueState(queue_id.clone()))
            .unwrap_or_else(|| MatchQueueState {
                queue_id: queue_id.clone(),
                players: Vec::new(&env),
                criteria_hash: criteria_hash.clone(),
            });

        // Prevent duplicate enqueue
        for existing in state.players.iter() {
            if existing == player {
                panic!("Player already in queue");
            }
        }

        state.players.push_back(player.clone());
        env.storage().persistent().set(&DataKey::QueueState(queue_id.clone()), &state);

        PlayerEnqueued { queue_id, player }.publish(&env);
    }

    /// Remove a player from a queue. Only admin or the player themselves can dequeue.
    pub fn dequeue_player(env: Env, caller: Address, queue_id: Symbol, player: Address) {
        caller.require_auth();
        let admin: Address =
            env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        assert!(caller == admin || caller == player, "Unauthorized");

        let mut state: MatchQueueState = env
            .storage()
            .persistent()
            .get(&DataKey::QueueState(queue_id.clone()))
            .expect("Queue not found");

        let mut found = false;
        let mut new_players = Vec::new(&env);
        for p in state.players.iter() {
            if p == player {
                found = true;
            } else {
                new_players.push_back(p);
            }
        }
        assert!(found, "Player not in queue");

        state.players = new_players;
        env.storage().persistent().set(&DataKey::QueueState(queue_id.clone()), &state);

        PlayerDequeued { queue_id, player }.publish(&env);
    }

    /// Create a match from a set of players. Admin-only.
    /// Players are removed from the queue on match creation.
    pub fn create_match(env: Env, queue_id: Symbol, players: Vec<Address>) -> u64 {
        let admin: Address =
            env.storage().instance().get(&DataKey::Admin).expect("Not initialized");
        admin.require_auth();

        assert!(!players.is_empty(), "Players list cannot be empty");

        let match_id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextMatchId)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::NextMatchId, &match_id.checked_add(1).expect("Overflow"));

        // Remove matched players from the queue
        let maybe_state: Option<MatchQueueState> = env
            .storage()
            .persistent()
            .get(&DataKey::QueueState(queue_id.clone()));

        if let Some(mut state) = maybe_state {
            let mut remaining = Vec::new(&env);
            for p in state.players.iter() {
                let mut matched = false;
                for mp in players.iter() {
                    if mp == p {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    remaining.push_back(p);
                }
            }
            state.players = remaining;
            env.storage().persistent().set(&DataKey::QueueState(queue_id.clone()), &state);
        }

        let record = MatchRecord {
            match_id,
            queue_id: queue_id.clone(),
            players,
        };
        env.storage().persistent().set(&DataKey::Match(match_id), &record);

        MatchCreated { match_id, queue_id }.publish(&env);

        match_id
    }

    /// Read the current state of a queue.
    pub fn queue_state(env: Env, queue_id: Symbol) -> MatchQueueState {
        env.storage()
            .persistent()
            .get(&DataKey::QueueState(queue_id))
            .expect("Queue not found")
    }

    /// Read a match record.
    pub fn match_state(env: Env, match_id: u64) -> MatchRecord {
        env.storage()
            .persistent()
            .get(&DataKey::Match(match_id))
            .expect("Match not found")
    }
}

// ── Tests ─────────────────────────────────────────────────────────
#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, vec, Env, Symbol};

    #[test]
    fn test_enqueue_and_create_match() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let p1 = Address::generate(&env);
        let p2 = Address::generate(&env);
        let queue_id = Symbol::new(&env, "ranked");
        let crit = Symbol::new(&env, "1v1");

        let contract_id = env.register_contract(None, MatchmakingQueue);
        let client = MatchmakingQueueClient::new(&env, &contract_id);

        client.init(&admin);
        client.enqueue_player(&queue_id, &p1, &crit);
        client.enqueue_player(&queue_id, &p2, &crit);

        let state = client.queue_state(&queue_id);
        assert_eq!(state.players.len(), 2);

        let players = vec![&env, p1.clone(), p2.clone()];
        let match_id = client.create_match(&queue_id, &players);
        assert_eq!(match_id, 0);

        // Queue should be empty now
        let state = client.queue_state(&queue_id);
        assert_eq!(state.players.len(), 0);
    }

    #[test]
    #[should_panic(expected = "Player already in queue")]
    fn test_duplicate_enqueue_fails() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let p1 = Address::generate(&env);
        let queue_id = Symbol::new(&env, "ranked");
        let crit = Symbol::new(&env, "1v1");

        let contract_id = env.register_contract(None, MatchmakingQueue);
        let client = MatchmakingQueueClient::new(&env, &contract_id);
        client.init(&admin);
        client.enqueue_player(&queue_id, &p1, &crit);
        client.enqueue_player(&queue_id, &p1, &crit);
    }

    #[test]
    fn test_dequeue_player() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let p1 = Address::generate(&env);
        let queue_id = Symbol::new(&env, "casual");
        let crit = Symbol::new(&env, "2v2");

        let contract_id = env.register_contract(None, MatchmakingQueue);
        let client = MatchmakingQueueClient::new(&env, &contract_id);
        client.init(&admin);
        client.enqueue_player(&queue_id, &p1, &crit);
        client.dequeue_player(&p1, &queue_id, &p1);

        let state = client.queue_state(&queue_id);
        assert_eq!(state.players.len(), 0);
    }

    #[test]
    #[should_panic(expected = "Already initialized")]
    fn test_double_init_fails() {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register_contract(None, MatchmakingQueue);
        let client = MatchmakingQueueClient::new(&env, &contract_id);
        client.init(&admin);
        client.init(&admin);
    }
}
