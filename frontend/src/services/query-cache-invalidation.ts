import type {
  CacheEntry,
  CacheInvalidationEvent,
  QueryCacheSnapshot,
  QueryKey,
  QueryPolicy,
} from "../types/query-cache";
import type { AppError } from "../types/errors";
import { toAppError, validatePreconditions } from "../utils/v1/errorMapper";

type Subscriber = (evt: {
  key: QueryKey;
  entry: CacheEntry<unknown> | null;
}) => void;

export const QueryKeys = {
  balances: {
    root: (): QueryKey => ["balances", "root"],
    account: (address: string): QueryKey => ["balances", "account", address],
  },
  games: {
    root: (): QueryKey => ["games", "root"],
    byId: (gameId: string | number): QueryKey => [
      "games",
      "byId",
      String(gameId),
    ],
    recentByAddress: (address: string): QueryKey => [
      "games",
      "recentByAddress",
      address,
    ],
  },
  rewards: {
    root: (): QueryKey => ["rewards", "root"],
    byAddress: (address: string): QueryKey => ["rewards", "byAddress", address],
  },
  profile: {
    root: (): QueryKey => ["profile", "root"],
    byAddress: (address: string): QueryKey => ["profile", "byAddress", address],
  },
} as const;

export const QueryPolicies: Record<string, QueryPolicy> = {
  balances: { staleTimeMs: 3_000, refetchOnInvalidate: true },
  games: { staleTimeMs: 10_000, refetchOnInvalidate: true },
  rewards: { staleTimeMs: 5_000, refetchOnInvalidate: true },
  profile: { staleTimeMs: 60_000, refetchOnInvalidate: true },
};

export class QueryCacheInvalidationError extends Error {
  readonly appError: AppError;

  constructor(appError: AppError) {
    super(appError.message);
    this.name = "QueryCacheInvalidationError";
    this.appError = appError;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

function now() {
  return Date.now();
}

function keyToString(key: QueryKey): string {
  return JSON.stringify(key);
}

function hasPrefix(key: QueryKey, prefix: QueryKey): boolean {
  if (prefix.length > key.length) return false;
  for (let i = 0; i < prefix.length; i++) {
    if (key[i] !== prefix[i]) return false;
  }
  return true;
}

export interface QueryCacheOptions {
  /** Optional upper bound to prevent unbounded growth. */
  maxEntries?: number;
}

export type Fetcher<T> = () => Promise<T>;

export class QueryCache {
  private readonly entries = new Map<string, CacheEntry<unknown>>();
  private readonly fetchers = new Map<string, Fetcher<unknown>>();
  private readonly subscribers: Set<Subscriber> = new Set();
  private readonly maxEntries: number | undefined;

  constructor(opts?: QueryCacheOptions) {
    this.maxEntries = opts?.maxEntries;
  }

  subscribe(fn: Subscriber) {
    this.subscribers.add(fn);
    return () => this.subscribers.delete(fn);
  }

  private notify(key: QueryKey, entry: CacheEntry<unknown> | null) {
    for (const s of this.subscribers) {
      try {
        s({ key, entry });
      } catch {
        // swallow
      }
    }
  }

  snapshot(): QueryCacheSnapshot {
    const keys = Array.from(this.entries.values()).map((e) => e.key);
    return { keys, size: this.entries.size };
  }

  registerFetcher<T>(key: QueryKey, fetcher: Fetcher<T>) {
    this.fetchers.set(keyToString(key), fetcher as Fetcher<unknown>);
  }

  unregisterFetcher(key: QueryKey) {
    this.fetchers.delete(keyToString(key));
  }

  get<T>(key: QueryKey): CacheEntry<T> | null {
    const e = this.entries.get(keyToString(key));
    return (e as CacheEntry<T> | undefined) ?? null;
  }

  set<T>(key: QueryKey, data: T, policy?: QueryPolicy): CacheEntry<T> {
    const ts = now();
    const existing = this.get<T>(key);

    const effectivePolicy = policy ??
      QueryPolicies[String(key[0])] ?? {
        staleTimeMs: 30_000,
        refetchOnInvalidate: true,
      };

    const entry: CacheEntry<T> = {
      key,
      data,
      policy: effectivePolicy,
      meta: {
        createdAt: existing?.meta.createdAt ?? ts,
        updatedAt: ts,
        staleAt: ts + effectivePolicy.staleTimeMs,
        invalidatedAt: existing?.meta.invalidatedAt,
      },
    };

    this.entries.set(keyToString(key), entry as CacheEntry<unknown>);
    this.enforceMaxEntries();
    this.notify(key, entry as CacheEntry<unknown>);
    return entry;
  }

  private enforceMaxEntries() {
    if (!this.maxEntries) return;
    if (this.entries.size <= this.maxEntries) return;

    const all = Array.from(this.entries.values());
    all.sort((a, b) => a.meta.updatedAt - b.meta.updatedAt);

    const toRemove = this.entries.size - this.maxEntries;
    for (let i = 0; i < toRemove; i++) {
      const victim = all[i];
      if (!victim) continue;
      this.entries.delete(keyToString(victim.key));
    }
  }

  isStale(key: QueryKey): boolean {
    const entry = this.get(key);
    if (!entry) return true;
    const ts = now();
    if (entry.meta.invalidatedAt !== undefined) return true;
    return ts >= entry.meta.staleAt;
  }

  /**
   * Reads from cache; if stale or missing, uses registered fetcher.
   * Deterministic transitions:
   * - success: cache is updated atomically
   * - failure: cache is NOT mutated
   */
  async getOrFetch<T>(
    key: QueryKey,
  ): Promise<{ data: T } | { error: AppError }> {
    try {
      const existing = this.get<T>(key);
      if (existing && !this.isStale(key)) return { data: existing.data };

      const fetcher = this.fetchers.get(keyToString(key)) as
        | Fetcher<T>
        | undefined;
      if (!fetcher) {
        return {
          error: toAppError(new Error("Missing fetcher"), undefined, {
            key,
          }),
        };
      }

      const data = await fetcher();
      this.set<T>(key, data);
      return { data };
    } catch (e) {
      return { error: toAppError(e, undefined, { key }) };
    }
  }

  invalidate(key: QueryKey, _evt?: CacheInvalidationEvent) {
    const e = this.entries.get(keyToString(key));
    if (!e) return;

    const updated: CacheEntry<unknown> = {
      ...e,
      meta: {
        ...e.meta,
        invalidatedAt: now(),
      },
    };

    this.entries.set(keyToString(key), updated);
    this.notify(key, updated);

    if (updated.policy.refetchOnInvalidate) {
      void this.refetch(key);
    }
  }

  invalidatePrefix(prefix: QueryKey, evt?: CacheInvalidationEvent) {
    for (const entry of this.entries.values()) {
      if (hasPrefix(entry.key, prefix)) {
        this.invalidate(entry.key, evt);
      }
    }
  }

  remove(key: QueryKey) {
    this.entries.delete(keyToString(key));
    this.notify(key, null);
  }

  clear() {
    for (const entry of this.entries.values()) {
      this.notify(entry.key, null);
    }
    this.entries.clear();
  }

  /**
   * Refetch uses registered fetcher, if any. On success, replaces cache entry.
   * On failure, keeps existing data but marks invalidated.
   */
  async refetch<T>(key: QueryKey): Promise<{ data: T } | { error: AppError }> {
    const fetcher = this.fetchers.get(keyToString(key)) as
      | Fetcher<T>
      | undefined;
    if (!fetcher) {
      return {
        error: toAppError(new Error("Missing fetcher"), undefined, {
          key,
        }),
      };
    }

    try {
      const data = await fetcher();
      this.set<T>(key, data);
      return { data };
    } catch (e) {
      this.invalidate(key, {
        at: now(),
        reason: "consistency_check",
      });
      return { error: toAppError(e, undefined, { key }) };
    }
  }

  /**
   * Cache consistency check: if a dependent key is missing/stale, invalidate the parent.
   */
  ensureDependencies(
    parent: QueryKey,
    deps: QueryKey[],
  ): { ok: true } | { ok: false; error: AppError } {
    try {
      for (const dep of deps) {
        if (this.isStale(dep)) {
          this.invalidate(parent, { at: now(), reason: "consistency_check" });
          return {
            ok: false,
            error: toAppError(new Error("Dependency stale"), "unknown", {
              parent,
              dep,
            }),
          };
        }
      }
      return { ok: true };
    } catch (e) {
      return { ok: false, error: toAppError(e) };
    }
  }
}

export type TxOutcome =
  | { ok: true; txHash?: string }
  | { ok: false; error: AppError; retryable: boolean };

export interface InvalidationRuleInput {
  outcome: TxOutcome;
  event: CacheInvalidationEvent;
}

export class QueryCacheInvalidator {
  constructor(private readonly cache: QueryCache) {}

  private invalidateBalances(addresses: string[], evt: CacheInvalidationEvent) {
    for (const addr of addresses) {
      this.cache.invalidate(QueryKeys.balances.account(addr), evt);
    }
  }

  private invalidateCoinFlip(
    addresses: string[],
    gameId: string | number | bigint | undefined,
    evt: CacheInvalidationEvent,
  ) {
    if (gameId !== undefined) {
      this.cache.invalidate(QueryKeys.games.byId(String(gameId)), evt);
    }
    for (const addr of addresses) {
      this.cache.invalidate(QueryKeys.games.recentByAddress(addr), evt);
    }
  }

  private invalidateAchievementBadge(
    addresses: string[],
    evt: CacheInvalidationEvent,
  ) {
    for (const addr of addresses) {
      this.cache.invalidate(QueryKeys.rewards.byAddress(addr), evt);
      this.cache.invalidate(QueryKeys.profile.byAddress(addr), evt);
    }
  }

  /**
   * Invalidation rules for StellarCade core domains.
   * These rules are intentionally conservative and can be extended.
   */
  applyRules(input: InvalidationRuleInput) {
    let reason: CacheInvalidationEvent["reason"];
    if (input.outcome.ok) {
      reason = "tx_success";
    } else if (input.outcome.retryable) {
      reason = "tx_failed_retryable";
    } else {
      reason = "tx_failed_terminal";
    }

    const evt: CacheInvalidationEvent = {
      ...input.event,
      at: now(),
      reason,
      ...(input.event.contractTx
        ? {
            contractTx: {
              ...input.event.contractTx,
              txHash: input.outcome.ok
                ? input.outcome.txHash
                : input.event.contractTx.txHash,
            },
          }
        : {}),
    };

    const addresses =
      evt.contractTx?.addresses ?? evt.mutation?.addresses ?? [];
    const gameId = evt.contractTx?.gameId;

    // Balances are critical after any on-chain state mutation.
    this.invalidateBalances(addresses, evt);

    // PrizePool state changes after pool mutations.
    if (evt.contractTx?.contract === "prizePool") {
      this.cache.invalidatePrefix(QueryKeys.balances.root(), evt);
    }

    // Game-related updates
    if (evt.contractTx?.contract === "coinFlip") {
      this.invalidateCoinFlip(addresses, gameId, evt);
    }

    // Rewards and profile may be affected by game completion.
    if (evt.contractTx?.contract === "achievementBadge") {
      this.invalidateAchievementBadge(addresses, evt);
    }
  }

  validateOrThrowPreconditions(opts: {
    /** True if wallet is currently connected. */
    walletConnected?: boolean;
    expectedNetwork?: string;
    currentNetwork?: string;
    contractAddress?: string;
    context?: Record<string, unknown>;
  }) {
    const err: AppError | null = validatePreconditions({
      requireWallet: opts.walletConnected === false,
      expectedNetwork: opts.expectedNetwork,
      currentNetwork: opts.currentNetwork,
      contractAddress: opts.contractAddress,
    });

    if (err) {
      throw new QueryCacheInvalidationError(
        toAppError(err, undefined, {
          ...opts.context,
        }),
      );
    }
  }
}

// ---------------------------------------------------------------------------
// Optimistic game mutations (generic apply / revert / finalize)
// ---------------------------------------------------------------------------

export interface OptimisticMutationRecord<T = unknown> {
  key: QueryKey;
  generation: number;
  previous: T | null;
  appliedAt: number;
}

/**
 * Coordinates optimistic cache updates for game actions with rollback and finalize.
 * Generic over cache value shape; safe to reuse for multiple mutation types.
 */
export class OptimisticGameMutationHelper {
  private readonly snapshots = new Map<string, OptimisticMutationRecord<unknown>>();
  private generation = 0;

  constructor(private readonly cache: QueryCache) {}

  /**
   * Apply optimistic data; snapshots prior value once per key until revert/finalize.
   * @returns Monotonic generation for race handling (revertIfLatest).
   */
  apply<T>(key: QueryKey, optimisticData: T, policy?: QueryPolicy): number {
    const ks = keyToString(key);
    if (!this.snapshots.has(ks)) {
      const prev = this.cache.get<T>(key)?.data ?? null;
      this.snapshots.set(ks, {
        key,
        generation: 0,
        previous: prev,
        appliedAt: Date.now(),
      });
    }
    this.generation += 1;
    const snap = this.snapshots.get(ks)!;
    snap.generation = this.generation;
    this.cache.set(key, optimisticData, policy);
    return this.generation;
  }

  revert(key: QueryKey): void {
    const ks = keyToString(key);
    const snap = this.snapshots.get(ks);
    if (!snap) return;
    if (snap.previous === null) {
      this.cache.remove(key);
    } else {
      this.cache.set(key, snap.previous as never);
    }
    this.snapshots.delete(ks);
  }

  /**
   * Revert only if this generation is still the active optimistic write (latest wins).
   */
  revertIfLatest(key: QueryKey, generation: number): void {
    const snap = this.snapshots.get(keyToString(key));
    if (!snap || snap.generation !== generation) return;
    this.revert(key);
  }

  /**
   * Drop optimistic snapshot and optionally set final data or refetch via fetcher.
   */
  async finalize<T>(
    key: QueryKey,
    finalData?: T,
    fetcher?: Fetcher<T>,
  ): Promise<{ data: T } | { error: AppError }> {
    this.snapshots.delete(keyToString(key));
    if (finalData !== undefined) {
      this.cache.set(key, finalData);
      return { data: finalData };
    }
    if (fetcher) {
      this.cache.registerFetcher(key, fetcher);
      return this.cache.refetch<T>(key);
    }
    return {
      error: toAppError(
        new Error("finalize requires finalData or fetcher"),
        undefined,
        { key },
      ),
    };
  }
}
