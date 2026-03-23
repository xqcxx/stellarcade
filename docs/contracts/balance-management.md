# balance-management

## Public Methods

### `update_balance`
Update user balance (Internal use by other contracts).

```rust
pub fn update_balance(_env: Env, _user: Address, _amount: i128, _is_add: bool)
```

### `get_balance`
View user balance.

```rust
pub fn get_balance(_env: Env, _user: Address) -> i128
```

