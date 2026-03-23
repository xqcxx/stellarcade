# trivia-game

## Public Methods

### `submit_answer`
Submit an answer for a specific question ID.

```rust
pub fn submit_answer(_env: Env, player: Address, _question_id: u32, _answer: String)
```

### `claim_reward`
Claim rewards for a correct answer.

```rust
pub fn claim_reward(_env: Env, player: Address, _game_id: u32)
```

