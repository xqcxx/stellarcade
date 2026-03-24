# pattern-puzzle

Metadata and accumulated state for a puzzle round.

## Public Methods

### `init`
Initialize the contract. May only be called once.  Stores the admin, prize pool contract address, and balance contract address in instance storage. Subsequent calls are rejected with `NotAuthorized`.

```rust
pub fn init(env: Env, admin: Address, prize_pool_contract: Address, balance_contract: Address) -> Result<(), Error>
```

### `create_puzzle`
Open a new puzzle round with a committed pattern hash. Admin only.  `pattern_commitment` is `SHA-256(correct_pattern_bytes)` computed off-chain. `entry_fee` is the token amount each player must wager (0 for free rounds). Round data is stored in persistent storage with a 30-day TTL.

```rust
pub fn create_puzzle(env: Env, admin: Address, round_id: u32, pattern_commitment: BytesN<32>, entry_fee: i128) -> Result<(), Error>
```

### `submit_solution`
Submit a solution guess for an open round.  Each player may submit exactly once per round, up to `MAX_PLAYERS_PER_ROUND` total. The `solution` bytes are stored and compared byte-for-byte against the revealed pattern during `resolve_round`. Entry fee accounting is updated here; actual token transfer should invoke the balance contract (see TODO below).

```rust
pub fn submit_solution(env: Env, player: Address, round_id: u32, solution: Bytes) -> Result<(), Error>
```

### `resolve_round`
Reveal the correct pattern, verify the commitment, and determine winners.  Admin only. `correct_pattern` must satisfy `SHA-256(correct_pattern) == stored pattern_commitment`. Iterates all submissions (bounded by `MAX_PLAYERS_PER_ROUND`) to mark winners and compute `winner_count`. Transitions the round to `Resolved`.

```rust
pub fn resolve_round(env: Env, admin: Address, round_id: u32, correct_pattern: Bytes) -> Result<(), Error>
```

### `claim_reward`
Claim the proportional reward share for a winning submission.  Returns the reward amount (`total_pot / winner_count`). The `Claimed` flag is set before any external call to preserve reentrancy safety. Actual token transfer should invoke the prize pool contract (see TODO below).

```rust
pub fn claim_reward(env: Env, player: Address, round_id: u32) -> Result<i128, Error>
```

### `get_round`
Returns round metadata, or `None` if the round does not exist.

```rust
pub fn get_round(env: Env, round_id: u32) -> Option<RoundData>
```

### `get_submission`
Returns the stored submission for a player in a round, or `None`.

```rust
pub fn get_submission(env: Env, round_id: u32, player: Address) -> Option<PlayerSubmission>
```

### `has_claimed`
Returns `true` if the player has already claimed their reward for a round.

```rust
pub fn has_claimed(env: Env, round_id: u32, player: Address) -> bool
```

### `get_leaderboard`
Returns a leaderboard snapshot (addresses of players) for a given round. Supports an optional limit for pagination or "top N" views. Stable ordering is guaranteed by the underlying Vec storage.

```rust
pub fn get_leaderboard(env: Env, round_id: u32, limit: Option<u32>) -> Vec<Address>
```

