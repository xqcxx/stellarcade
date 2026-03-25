/**
 * Optimistic mutation helper + orchestrator integration tests (#219).
 */

import GlobalStateStore from "@/services/global-state-store";
import {
  OptimisticGameMutationHelper,
  QueryCache,
  QueryKeys,
} from "@/services/query-cache-invalidation";
import TransactionOrchestrator, {
  executeGameActionWithOptimistic,
} from "@/services/transaction-orchestrator";
import { ConfirmationStatus } from "@/types/transaction-orchestrator";
import { describe, it, expect, beforeEach } from "vitest";

describe("OptimisticGameMutationHelper", () => {
  let cache: QueryCache;
  let helper: OptimisticGameMutationHelper;

  beforeEach(() => {
    cache = new QueryCache();
    helper = new OptimisticGameMutationHelper(cache);
  });

  it("apply sets optimistic value", () => {
    const key = QueryKeys.games.byId("1");
    helper.apply(key, { score: 99 });
    expect(cache.get(key)?.data).toEqual({ score: 99 });
  });

  it("revert restores previous cache value", () => {
    const key = QueryKeys.games.byId("2");
    cache.set(key, { score: 10 });
    helper.apply(key, { score: 50 });
    expect(cache.get(key)?.data).toEqual({ score: 50 });
    helper.revert(key);
    expect(cache.get(key)?.data).toEqual({ score: 10 });
  });

  it("finalize replaces with final data", async () => {
    const key = QueryKeys.games.byId("3");
    helper.apply(key, { score: 1 });
    const out = await helper.finalize(key, { score: 42 });
    expect(out).toEqual({ data: { score: 42 } });
    expect(cache.get(key)?.data).toEqual({ score: 42 });
  });

  it("revertIfLatest ignores stale generation (race)", () => {
    const key = QueryKeys.games.byId("4");
    cache.set(key, { v: 0 });
    const g1 = helper.apply(key, { v: 1 });
    helper.apply(key, { v: 2 });
    helper.revertIfLatest(key, g1);
    expect(cache.get(key)?.data).toEqual({ v: 2 });
  });

  it("revertIfLatest reverts when generation matches", () => {
    const key = QueryKeys.games.byId("5");
    cache.set(key, { v: 0 });
    const g = helper.apply(key, { v: 1 });
    helper.revertIfLatest(key, g);
    expect(cache.get(key)?.data).toEqual({ v: 0 });
  });
});

describe("executeGameActionWithOptimistic", () => {
  it("rolls back optimistic state on failed execute", async () => {
    const cache = new QueryCache();
    const key = QueryKeys.games.byId("x");
    cache.set(key, { ok: true });
    const helper = new OptimisticGameMutationHelper(cache);
    const orch = new TransactionOrchestrator({
      now: () => 0,
      sleep: async () => {},
      generateCorrelationId: () => "cid",
    });

    const result = await executeGameActionWithOptimistic({
      orchestrator: orch,
      optimistic: helper,
      cacheKey: key,
      optimisticData: { ok: false },
      request: {
        operation: "test",
        input: {},
        submit: async () => {
          throw new Error("fail");
        },
        confirm: async () => ({ status: ConfirmationStatus.CONFIRMED }),
      },
    });

    expect(result.success).toBe(false);
    expect(cache.get(key)?.data).toEqual({ ok: true });
  });

  it("finalizes on success with buildFinalize", async () => {
    const cache = new QueryCache();
    const key = QueryKeys.games.byId("y");
    const helper = new OptimisticGameMutationHelper(cache);
    const orch = new TransactionOrchestrator({
      now: () => 0,
      sleep: async () => {},
      generateCorrelationId: () => "cid2",
    });

    const result = await executeGameActionWithOptimistic({
      orchestrator: orch,
      optimistic: helper,
      cacheKey: key,
      optimisticData: { phase: "pending" },
      buildFinalize: () => ({ phase: "done" }),
      request: {
        operation: "okop",
        input: {},
        submit: async () => ({ txHash: "abc", data: { done: true } }),
        confirm: async () => ({
          status: ConfirmationStatus.CONFIRMED,
          confirmations: 1,
        }),
        pollIntervalMs: 1,
        confirmationTimeoutMs: 5_000,
      },
    });

    expect(result.success).toBe(true);
    expect(cache.get(key)?.data).toEqual({ phase: "done" });
  });
});

describe("GlobalStateStore optimistic patches", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("applies and reverts optimistic patches without persisting them", () => {
    const store = new GlobalStateStore({ storageKey: "opt_test" });
    store.dispatch({
      type: "OPTIMISTIC_PATCH",
      payload: { key: "game:1", value: { optimistic: true } },
    });
    expect(store.getState().optimisticPatches["game:1"]).toEqual({
      optimistic: true,
    });
    store.dispatch({ type: "OPTIMISTIC_REVERT", payload: { key: "game:1" } });
    expect(store.getState().optimisticPatches["game:1"]).toBeUndefined();

    store.dispatch({
      type: "AUTH_SET",
      payload: { userId: "u", token: "t" },
    });
    const raw = JSON.parse(localStorage.getItem("opt_test") as string);
    expect(raw.optimisticPatches).toBeUndefined();
  });
});
