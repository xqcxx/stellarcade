//! Stellarcade Pattern Puzzle Contract
//!
//! Implements a commit-reveal puzzle game where players submit pattern guesses
//! and winners share the prize pot after the admin reveals the correct answer.
//!
//! ## Game Flow
//! 1. Admin calls `create_puzzle` with SHA-256(correct_pattern) as commitment.
//! 2. Players call `submit_solution` with their guesses and pay the entry fee.
//! 3. Admin calls `resolve_round` with the plaintext answer (verified against hash).
//! 4. Winning players call `claim_reward` to receive their proportional share.
//!
//! ## Storage Strategy
//! - `instance()` storage: contract-level config only (Admin, PrizePoolContract,
//!   BalanceContract). Small, fixed size, bounded.
//! - `persistent()` storage: all per-round and per-player data (Round, Players,
//!   Submission, IsWinner, Claimed). Each key is an independent ledger entry
//!   with constant-cost access and an explicit TTL extended on every write.
#![no_std]
#![allow(unexpected_cfgs)]

use soroban_sdk::{
    contract, contracterror, contractevent, contractimpl, contracttype, Address, Bytes, BytesN,
    Env, Vec,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of players allowed per round.
/// Bounds the O(n) iteration in resolve_round to a known gas budget.
pub const MAX_PLAYERS_PER_ROUND: u32 = 500;

/// Persistent storage TTL in ledgers (~30 days at 5s/ledger).
/// Extended on every write so active round data never expires mid-game.
pub const PERSISTENT_BUMP_LEDGERS: u32 = 518_400;

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum Error {
    NotInitialized     = 1,
    NotAuthorized      = 2,
    RoundNotFound      = 3,
    RoundAlreadyExists = 4,
    RoundNotOpen       = 5,
    RoundNotResolved   = 6,
    AlreadySubmitted   = 7,
    AlreadyClaimed     = 8,
    NoRewardAvailable  = 9,
    InvalidAmount      = 10,
    Overflow           = 11,
    CommitmentMismatch = 12,
    RoundFull          = 13,
}

// ---------------------------------------------------------------------------
// Storage Types
// ---------------------------------------------------------------------------

/// Round lifecycle state machine: Open → Resolved.
/// Per-player claimed state is tracked separately via DataKey::Claimed.
#[contracttype]
#[derive(Clone, PartialEq)]
pub enum RoundStatus {
    Open     = 0,
    Resolved = 1,
}

/// Metadata and accumulated state for a puzzle round.
#[contracttype]
#[derive(Clone)]
pub struct RoundData {
    /// SHA-256 hash of the correct pattern, committed before submissions open.
    pub pattern_commitment: BytesN<32>,
    pub status:             RoundStatus,
    /// Set during resolve_round after iterating all submissions.
    pub winner_count:       u32,
    /// Populated only after resolve_round; empty Bytes while status is Open.
    pub correct_pattern:    Bytes,
    /// Amount each player must wager to enter (0 = free round).
    pub entry_fee:          i128,
    /// Accumulated from all submitted entry fees.
    pub total_pot:          i128,
    /// Current number of submissions; enforces MAX_PLAYERS_PER_ROUND.
    pub player_count:       u32,
}

/// A player's committed guess for a round.
#[contracttype]
#[derive(Clone)]
pub struct PlayerSubmission {
    pub solution: Bytes,
    pub wager:    i128,
}

/// Storage key discriminants.
///
/// Instance keys (Admin, PrizePoolContract, BalanceContract): contract config,
/// small fixed set, stored in a single ledger entry.
///
/// Persistent keys (Round, Players, Submission, IsWinner, Claimed): per-round
/// and per-player data, each stored as an independent ledger entry with its own
/// TTL so reads and writes are O(1) and cost does not scale with contract state.
#[contracttype]
pub enum DataKey {
    // --- instance() keys: contract-level config ---
    Admin,
    PrizePoolContract,
    BalanceContract,
    // --- persistent() keys: round and player data ---
    /// RoundData keyed by round_id.
    Round(u32),
    /// Vec<Address> of all submitters for a round, used during resolve_round.
    Players(u32),
    /// PlayerSubmission keyed by (round_id, player).
    Submission(u32, Address),
    /// Set to `true` during resolve_round for each correct submitter.
    IsWinner(u32, Address),
    /// Set to `true` during claim_reward to prevent double-claims.
    Claimed(u32, Address),
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[contractevent]
pub struct RoundCreated {
    #[topic]
    pub round_id: u32,
    pub pattern_commitment: BytesN<32>,
}

#[contractevent]
pub struct SolutionSubmitted {
    #[topic]
    pub player: Address,
    #[topic]
    pub round_id: u32,
    pub solution: Bytes,
}

#[contractevent]
pub struct RoundResolved {
    #[topic]
    pub round_id: u32,
    pub correct_pattern: Bytes,
    pub winner_count: u32,
}

#[contractevent]
pub struct RewardClaimed {
    #[topic]
    pub player: Address,
    #[topic]
    pub round_id: u32,
    pub amount: i128,
}

// ---------------------------------------------------------------------------
// Contract
// ---------------------------------------------------------------------------

#[contract]
pub struct PatternPuzzle;

#[contractimpl]
impl PatternPuzzle {
    // -----------------------------------------------------------------------
    // init
    // -----------------------------------------------------------------------

    /// Initialize the contract. May only be called once.
    ///
    /// Stores the admin, prize pool contract address, and balance contract
    /// address in instance storage. Subsequent calls are rejected with
    /// `NotAuthorized`.
    pub fn init(
        env:                 Env,
        admin:               Address,
        prize_pool_contract: Address,
        balance_contract:    Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::NotAuthorized);
        }

        admin.require_auth();

        env.storage().instance().set(&DataKey::Admin,             &admin);
        env.storage().instance().set(&DataKey::PrizePoolContract, &prize_pool_contract);
        env.storage().instance().set(&DataKey::BalanceContract,   &balance_contract);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // create_puzzle
    // -----------------------------------------------------------------------

    /// Open a new puzzle round with a committed pattern hash. Admin only.
    ///
    /// `pattern_commitment` is `SHA-256(correct_pattern_bytes)` computed off-chain.
    /// `entry_fee` is the token amount each player must wager (0 for free rounds).
    /// Round data is stored in persistent storage with a 30-day TTL.
    pub fn create_puzzle(
        env:                Env,
        admin:              Address,
        round_id:           u32,
        pattern_commitment: BytesN<32>,
        entry_fee:          i128,
    ) -> Result<(), Error> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        admin.require_auth();
        if admin != stored_admin {
            return Err(Error::NotAuthorized);
        }

        if env.storage().persistent().has(&DataKey::Round(round_id)) {
            return Err(Error::RoundAlreadyExists);
        }

        if entry_fee < 0 {
            return Err(Error::InvalidAmount);
        }

        let round = RoundData {
            pattern_commitment: pattern_commitment.clone(),
            status:             RoundStatus::Open,
            winner_count:       0,
            correct_pattern:    Bytes::new(&env),
            entry_fee,
            total_pot:          0,
            player_count:       0,
        };

        env.storage().persistent().set(&DataKey::Round(round_id), &round);
        env.storage().persistent().extend_ttl(
            &DataKey::Round(round_id),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        env.storage().persistent().set(&DataKey::Players(round_id), &Vec::<Address>::new(&env));
        env.storage().persistent().extend_ttl(
            &DataKey::Players(round_id),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        RoundCreated { round_id, pattern_commitment }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // submit_solution
    // -----------------------------------------------------------------------

    /// Submit a solution guess for an open round.
    ///
    /// Each player may submit exactly once per round, up to `MAX_PLAYERS_PER_ROUND`
    /// total. The `solution` bytes are stored and compared byte-for-byte against the
    /// revealed pattern during `resolve_round`. Entry fee accounting is updated here;
    /// actual token transfer should invoke the balance contract (see TODO below).
    pub fn submit_solution(
        env:      Env,
        player:   Address,
        round_id: u32,
        solution: Bytes,
    ) -> Result<(), Error> {
        player.require_auth();

        let mut round: RoundData = env
            .storage()
            .persistent()
            .get(&DataKey::Round(round_id))
            .ok_or(Error::RoundNotFound)?;

        if round.status != RoundStatus::Open {
            return Err(Error::RoundNotOpen);
        }

        if round.player_count >= MAX_PLAYERS_PER_ROUND {
            return Err(Error::RoundFull);
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Submission(round_id, player.clone()))
        {
            return Err(Error::AlreadySubmitted);
        }

        if solution.is_empty() {
            return Err(Error::InvalidAmount);
        }

        let submission = PlayerSubmission {
            solution: solution.clone(),
            wager:    round.entry_fee,
        };
        env.storage()
            .persistent()
            .set(&DataKey::Submission(round_id, player.clone()), &submission);
        env.storage().persistent().extend_ttl(
            &DataKey::Submission(round_id, player.clone()),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        // TODO: Invoke balance_contract to transfer entry_fee from player to this contract.
        // balance_contract_client::new(&env, &balance_contract).transfer(&player, &env.current_contract_address(), &round.entry_fee);

        let mut players: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Players(round_id))
            .unwrap_or_else(|| Vec::new(&env));
        players.push_back(player.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Players(round_id), &players);
        env.storage().persistent().extend_ttl(
            &DataKey::Players(round_id),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        round.total_pot = round
            .total_pot
            .checked_add(round.entry_fee)
            .ok_or(Error::Overflow)?;
        round.player_count = round
            .player_count
            .checked_add(1)
            .ok_or(Error::Overflow)?;
        env.storage().persistent().set(&DataKey::Round(round_id), &round);
        env.storage().persistent().extend_ttl(
            &DataKey::Round(round_id),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        SolutionSubmitted { player, round_id, solution }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // resolve_round
    // -----------------------------------------------------------------------

    /// Reveal the correct pattern, verify the commitment, and determine winners.
    ///
    /// Admin only. `correct_pattern` must satisfy `SHA-256(correct_pattern) ==
    /// stored pattern_commitment`. Iterates all submissions (bounded by
    /// `MAX_PLAYERS_PER_ROUND`) to mark winners and compute `winner_count`.
    /// Transitions the round to `Resolved`.
    pub fn resolve_round(
        env:             Env,
        admin:           Address,
        round_id:        u32,
        correct_pattern: Bytes,
    ) -> Result<(), Error> {
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;

        admin.require_auth();
        if admin != stored_admin {
            return Err(Error::NotAuthorized);
        }

        let mut round: RoundData = env
            .storage()
            .persistent()
            .get(&DataKey::Round(round_id))
            .ok_or(Error::RoundNotFound)?;

        // Prevents double-resolve: resolved round has status != Open.
        if round.status != RoundStatus::Open {
            return Err(Error::RoundNotOpen);
        }

        // Commit-reveal verification.
        let revealed_hash: BytesN<32> = env.crypto().sha256(&correct_pattern).into();
        if revealed_hash != round.pattern_commitment {
            return Err(Error::CommitmentMismatch);
        }

        let players: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Players(round_id))
            .unwrap_or_else(|| Vec::new(&env));

        let mut winner_count: u32 = 0;

        // Bounded by MAX_PLAYERS_PER_ROUND enforced in submit_solution.
        for player in players.iter() {
            if let Some(submission) = env
                .storage()
                .persistent()
                .get::<DataKey, PlayerSubmission>(&DataKey::Submission(round_id, player.clone()))
            {
                if submission.solution == correct_pattern {
                    env.storage()
                        .persistent()
                        .set(&DataKey::IsWinner(round_id, player.clone()), &true);
                    env.storage().persistent().extend_ttl(
                        &DataKey::IsWinner(round_id, player.clone()),
                        PERSISTENT_BUMP_LEDGERS,
                        PERSISTENT_BUMP_LEDGERS,
                    );
                    winner_count = winner_count.checked_add(1).ok_or(Error::Overflow)?;
                }
            }
        }

        round.status          = RoundStatus::Resolved;
        round.correct_pattern = correct_pattern.clone();
        round.winner_count    = winner_count;
        env.storage().persistent().set(&DataKey::Round(round_id), &round);
        env.storage().persistent().extend_ttl(
            &DataKey::Round(round_id),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        RoundResolved { round_id, correct_pattern, winner_count }.publish(&env);

        Ok(())
    }

    // -----------------------------------------------------------------------
    // claim_reward
    // -----------------------------------------------------------------------

    /// Claim the proportional reward share for a winning submission.
    ///
    /// Returns the reward amount (`total_pot / winner_count`). The `Claimed` flag
    /// is set before any external call to preserve reentrancy safety. Actual token
    /// transfer should invoke the prize pool contract (see TODO below).
    pub fn claim_reward(
        env:      Env,
        player:   Address,
        round_id: u32,
    ) -> Result<i128, Error> {
        player.require_auth();

        let round: RoundData = env
            .storage()
            .persistent()
            .get(&DataKey::Round(round_id))
            .ok_or(Error::RoundNotFound)?;

        if round.status != RoundStatus::Resolved {
            return Err(Error::RoundNotResolved);
        }

        let already_claimed: bool = env
            .storage()
            .persistent()
            .get(&DataKey::Claimed(round_id, player.clone()))
            .unwrap_or(false);
        if already_claimed {
            return Err(Error::AlreadyClaimed);
        }

        let is_winner: bool = env
            .storage()
            .persistent()
            .get(&DataKey::IsWinner(round_id, player.clone()))
            .unwrap_or(false);
        if !is_winner {
            return Err(Error::NoRewardAvailable);
        }

        if round.winner_count == 0 {
            return Err(Error::NoRewardAvailable);
        }

        let reward: i128 = round
            .total_pot
            .checked_div(round.winner_count as i128)
            .ok_or(Error::Overflow)?;

        // Mark claimed before any external call (reentrancy safety).
        env.storage()
            .persistent()
            .set(&DataKey::Claimed(round_id, player.clone()), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::Claimed(round_id, player.clone()),
            PERSISTENT_BUMP_LEDGERS,
            PERSISTENT_BUMP_LEDGERS,
        );

        // TODO: Invoke prize_pool_contract to transfer `reward` tokens to `player`.
        // prize_pool_contract_client::new(&env, &prize_pool_contract).payout(&player, &reward);

        RewardClaimed { player, round_id, amount: reward }.publish(&env);

        Ok(reward)
    }

    // -----------------------------------------------------------------------
    // View functions
    // -----------------------------------------------------------------------

    /// Returns round metadata, or `None` if the round does not exist.
    pub fn get_round(env: Env, round_id: u32) -> Option<RoundData> {
        env.storage().persistent().get(&DataKey::Round(round_id))
    }

    /// Returns the stored submission for a player in a round, or `None`.
    pub fn get_submission(env: Env, round_id: u32, player: Address) -> Option<PlayerSubmission> {
        env.storage()
            .persistent()
            .get(&DataKey::Submission(round_id, player))
    }

    /// Returns `true` if the player has already claimed their reward for a round.
    pub fn has_claimed(env: Env, round_id: u32, player: Address) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Claimed(round_id, player))
            .unwrap_or(false)
    }

    /// Returns a leaderboard snapshot (addresses of players) for a given round.
    /// Supports an optional limit for pagination or "top N" views. Stable ordering
    /// is guaranteed by the underlying Vec storage.
    pub fn get_leaderboard(env: Env, round_id: u32, limit: Option<u32>) -> Vec<Address> {
        let players: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Players(round_id))
            .unwrap_or_else(|| Vec::new(&env));

        let l = limit.unwrap_or(players.len());
        let n = if l > players.len() { players.len() } else { l };
        
        let mut result = Vec::new(&env);
        for i in 0..n {
            result.push_back(players.get(i).unwrap());
        }
        result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::{testutils::Address as _, Bytes, BytesN, Env};

    /// Compute SHA-256 of a byte slice using the test environment's crypto host.
    fn sha256_of(env: &Env, data: &[u8]) -> BytesN<32> {
        let bytes = Bytes::from_slice(env, data);
        env.crypto().sha256(&bytes).into()
    }

    /// Register the contract and run `init`. Returns (client, admin, prize_pool, balance).
    fn setup(env: &Env) -> (PatternPuzzleClient<'_>, Address, Address, Address) {
        let contract_id = env.register(PatternPuzzle, ());
        let client = PatternPuzzleClient::new(env, &contract_id);
        let admin = Address::generate(env);
        let prize_pool = Address::generate(env);
        let balance = Address::generate(env);

        env.mock_all_auths();
        client.init(&admin, &prize_pool, &balance);

        (client, admin, prize_pool, balance)
    }

    // ------------------------------------------------------------------
    // 1. Happy path: create → submit (1 winner + 1 loser) → resolve → claim
    // ------------------------------------------------------------------

    #[test]
    fn test_full_happy_path() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct: &[u8] = b"BLUE-CIRCLE";
        let commitment: BytesN<32> = sha256_of(&env, correct);
        let round_id: u32 = 1;
        let entry_fee: i128 = 100;

        client.create_puzzle(&admin, &round_id, &commitment, &entry_fee);

        let winner = Address::generate(&env);
        let loser = Address::generate(&env);
        let correct_bytes = Bytes::from_slice(&env, correct);
        let wrong_bytes = Bytes::from_slice(&env, b"RED-SQUARE");

        client.submit_solution(&winner, &round_id, &correct_bytes);
        client.submit_solution(&loser, &round_id, &wrong_bytes);

        client.resolve_round(&admin, &round_id, &correct_bytes);

        let round = client.get_round(&round_id).unwrap();
        assert_eq!(round.winner_count, 1);
        assert_eq!(round.total_pot, 200); // 2 players × 100

        // Sole winner receives the entire pot.
        let reward = client.claim_reward(&winner, &round_id);
        assert_eq!(reward, 200);
        assert!(client.has_claimed(&round_id, &winner));
    }

    // ------------------------------------------------------------------
    // 2. Non-admin cannot create puzzle
    // ------------------------------------------------------------------

    #[test]
    fn test_create_puzzle_unauthorized() {
        let env = Env::default();
        let (client, _, _, _) = setup(&env);
        env.mock_all_auths();

        let imposter = Address::generate(&env);
        let commitment = sha256_of(&env, b"SECRET");
        let result = client.try_create_puzzle(&imposter, &1u32, &commitment, &100i128);

        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 3. Non-admin cannot resolve round
    // ------------------------------------------------------------------

    #[test]
    fn test_resolve_round_unauthorized() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let commitment = sha256_of(&env, b"PATTERN");
        client.create_puzzle(&admin, &1u32, &commitment, &50i128);

        let imposter = Address::generate(&env);
        let correct_pattern = Bytes::from_slice(&env, b"PATTERN");
        let result = client.try_resolve_round(&imposter, &1u32, &correct_pattern);

        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 4. Duplicate submission rejected
    // ------------------------------------------------------------------

    #[test]
    fn test_duplicate_submission_rejected() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let commitment = sha256_of(&env, b"ANSWER");
        client.create_puzzle(&admin, &1u32, &commitment, &10i128);

        let player = Address::generate(&env);
        let solution = Bytes::from_slice(&env, b"GUESS");
        client.submit_solution(&player, &1u32, &solution);

        let result = client.try_submit_solution(&player, &1u32, &solution);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 5. Loser cannot claim reward
    // ------------------------------------------------------------------

    #[test]
    fn test_loser_cannot_claim() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct = b"CORRECT";
        let commitment = sha256_of(&env, correct);
        client.create_puzzle(&admin, &1u32, &commitment, &10i128);

        let loser = Address::generate(&env);
        let wrong = Bytes::from_slice(&env, b"WRONG");
        client.submit_solution(&loser, &1u32, &wrong);

        let correct_bytes = Bytes::from_slice(&env, correct);
        client.resolve_round(&admin, &1u32, &correct_bytes);

        let result = client.try_claim_reward(&loser, &1u32);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 6. Double-claim prevented
    // ------------------------------------------------------------------

    #[test]
    fn test_double_claim_prevented() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct = b"WINNER";
        let commitment = sha256_of(&env, correct);
        client.create_puzzle(&admin, &1u32, &commitment, &50i128);

        let player = Address::generate(&env);
        let correct_bytes = Bytes::from_slice(&env, correct);
        client.submit_solution(&player, &1u32, &correct_bytes);
        client.resolve_round(&admin, &1u32, &correct_bytes);
        client.claim_reward(&player, &1u32);

        let result = client.try_claim_reward(&player, &1u32);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 7. Cannot submit to a resolved round
    // ------------------------------------------------------------------

    #[test]
    fn test_cannot_submit_after_resolve() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct = b"FINAL";
        let commitment = sha256_of(&env, correct);
        client.create_puzzle(&admin, &1u32, &commitment, &0i128);

        let correct_bytes = Bytes::from_slice(&env, correct);
        client.resolve_round(&admin, &1u32, &correct_bytes);

        let late_player = Address::generate(&env);
        let result = client.try_submit_solution(&late_player, &1u32, &correct_bytes);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 8. Cannot resolve an already-resolved round
    // ------------------------------------------------------------------

    #[test]
    fn test_cannot_double_resolve() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct = b"ONCE";
        let commitment = sha256_of(&env, correct);
        client.create_puzzle(&admin, &1u32, &commitment, &0i128);

        let correct_bytes = Bytes::from_slice(&env, correct);
        client.resolve_round(&admin, &1u32, &correct_bytes);

        let result = client.try_resolve_round(&admin, &1u32, &correct_bytes);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 9. Commitment mismatch is rejected during resolve
    // ------------------------------------------------------------------

    #[test]
    fn test_commitment_mismatch_rejected() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let commitment = sha256_of(&env, b"REAL_ANSWER");
        client.create_puzzle(&admin, &1u32, &commitment, &0i128);

        let wrong_reveal = Bytes::from_slice(&env, b"FAKE_ANSWER");
        let result = client.try_resolve_round(&admin, &1u32, &wrong_reveal);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 10. Multiple winners split the pot equally
    // ------------------------------------------------------------------

    #[test]
    fn test_reward_split_multiple_winners() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let correct = b"SHARED";
        let commitment = sha256_of(&env, correct);
        client.create_puzzle(&admin, &1u32, &commitment, &100i128);

        let winner1 = Address::generate(&env);
        let winner2 = Address::generate(&env);
        let correct_bytes = Bytes::from_slice(&env, correct);

        client.submit_solution(&winner1, &1u32, &correct_bytes);
        client.submit_solution(&winner2, &1u32, &correct_bytes);
        client.resolve_round(&admin, &1u32, &correct_bytes);

        let reward1 = client.claim_reward(&winner1, &1u32);
        let reward2 = client.claim_reward(&winner2, &1u32);

        assert_eq!(reward1, 100);
        assert_eq!(reward2, 100);
    }

    // ------------------------------------------------------------------
    // 11. Leaderboard snapshot with limit
    // ------------------------------------------------------------------

    #[test]
    fn test_leaderboard_snapshot() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let round_id: u32 = 1;
        client.create_puzzle(&admin, &round_id, &sha256_of(&env, b"A"), &0);

        let p1 = Address::generate(&env);
        let p2 = Address::generate(&env);
        let p3 = Address::generate(&env);

        client.submit_solution(&p1, &round_id, &Bytes::from_slice(&env, b"A"));
        client.submit_solution(&p2, &round_id, &Bytes::from_slice(&env, b"A"));
        client.submit_solution(&p3, &round_id, &Bytes::from_slice(&env, b"A"));

        // Full leaderboard
        let all = client.get_leaderboard(&round_id, &None);
        assert_eq!(all.len(), 3);
        assert_eq!(all.get(0).unwrap(), p1);
        assert_eq!(all.get(1).unwrap(), p2);
        assert_eq!(all.get(2).unwrap(), p3);

        // Limited leaderboard
        let top2 = client.get_leaderboard(&round_id, &Some(2));
        assert_eq!(top2.len(), 2);
        assert_eq!(top2.get(0).unwrap(), p1);
        assert_eq!(top2.get(1).unwrap(), p2);
    }

    // ------------------------------------------------------------------
    // 12. Cannot initialize contract twice
    // ------------------------------------------------------------------

    #[test]
    fn test_cannot_init_twice() {
        let env = Env::default();
        let (client, admin, prize_pool, balance) = setup(&env);
        env.mock_all_auths();

        let result = client.try_init(&admin, &prize_pool, &balance);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // 12. Round rejects submissions beyond MAX_PLAYERS_PER_ROUND cap
    // ------------------------------------------------------------------

    #[test]
    fn test_round_full_rejected() {
        let env = Env::default();
        let (client, admin, _, _) = setup(&env);
        env.mock_all_auths();

        let commitment = sha256_of(&env, b"CAP_TEST");
        client.create_puzzle(&admin, &1u32, &commitment, &0i128);

        // Fill the round to capacity.
        for _ in 0..MAX_PLAYERS_PER_ROUND {
            let player = Address::generate(&env);
            let solution = Bytes::from_slice(&env, b"GUESS");
            client.submit_solution(&player, &1u32, &solution);
        }

        // One more submission must be rejected with RoundFull.
        let overflow_player = Address::generate(&env);
        let solution = Bytes::from_slice(&env, b"GUESS");
        let result = client.try_submit_solution(&overflow_player, &1u32, &solution);
        assert!(result.is_err());
    }
}
