# random-generator

A pending randomness request registered by an authorized game contract.

## Public Methods

### `init`
Initialize the contract. May only be called once.  `oracle` is the sole address permitted to call `fulfill_random`. It is expected to be a backend service that pre-commits server seeds off-chain before each game round begins.

```rust
pub fn init(env: Env, admin: Address, oracle: Address) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `admin` | `Address` |
| `oracle` | `Address` |

#### Return Type

`Result<(), Error>`

### `authorize`
Add a game contract to the caller whitelist. Admin only.

```rust
pub fn authorize(env: Env, admin: Address, caller: Address) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `admin` | `Address` |
| `caller` | `Address` |

#### Return Type

`Result<(), Error>`

### `revoke`
Remove a game contract from the caller whitelist. Admin only.

```rust
pub fn revoke(env: Env, admin: Address, caller: Address) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `admin` | `Address` |
| `caller` | `Address` |

#### Return Type

`Result<(), Error>`

### `request_random`
Submit a randomness request. Only whitelisted callers may call this.  `max` must be >= 2. The fulfilled result will be in `[0, max - 1]`. `request_id` must be globally unique — rejected if a pending or fulfilled entry for the same ID already exists.

```rust
pub fn request_random(env: Env, caller: Address, request_id: u64, max: u64) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `caller` | `Address` |
| `request_id` | `u64` |
| `max` | `u64` |

#### Return Type

`Result<(), Error>`

### `fulfill_random`
Fulfill a pending randomness request. Oracle only.  The result is derived as: `sha256(server_seed || request_id_be_bytes)[0..8] % max`  Both `server_seed` and `result` are persisted for on-chain verification. Fairness holds when the oracle published `sha256(server_seed)` before the corresponding `request_random` call was submitted.

```rust
pub fn fulfill_random(env: Env, oracle: Address, request_id: u64, server_seed: BytesN<32>) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `oracle` | `Address` |
| `request_id` | `u64` |
| `server_seed` | `BytesN<32>` |

#### Return Type

`Result<(), Error>`

### `set_entropy_metadata`
Set entropy source version metadata. Admin only.  Metadata is informational and does not affect randomness output.

```rust
pub fn set_entropy_metadata(env: Env, admin: Address, metadata: EntropySourceMetadata) -> Result<(), Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `admin` | `Address` |
| `metadata` | `EntropySourceMetadata` |

#### Return Type

`Result<(), Error>`

### `get_entropy_metadata`
Read the current entropy source version metadata.

```rust
pub fn get_entropy_metadata(env: Env) -> Result<EntropySourceMetadata, Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |

#### Return Type

`Result<EntropySourceMetadata, Error>`

### `get_result`
Return the fulfilled result for a `request_id`.  Returns `RequestNotFound` if the request is still pending or never existed.

```rust
pub fn get_result(env: Env, request_id: u64) -> Result<FulfilledEntry, Error>
```

#### Parameters

| Name | Type |
|------|------|
| `env` | `Env` |
| `request_id` | `u64` |

#### Return Type

`Result<FulfilledEntry, Error>`

