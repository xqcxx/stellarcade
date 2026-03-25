/**
 * Transaction Orchestrator.
 *
 * Unified lifecycle manager for side-effectful transactions:
 * validate -> submit -> confirm with retry and deterministic transitions.
 */

import type { QueryKey } from "../types/query-cache";
import { toAppError } from "../utils/v1/errorMapper";
import { ErrorDomain, ErrorSeverity, type AppError } from "../types/errors";
import {
  ConfirmationStatus,
  OrchestratorErrorCode,
  TransactionPhase,
  TERMINAL_TRANSACTION_PHASES,
  type OrchestratorError,
  type RetryPolicy,
  type TransactionContext,
  type TransactionOrchestratorState,
  type TransactionRequest,
  type TransactionResult,
  type TransactionStateSubscriber,
} from "../types/transaction-orchestrator";
import type { Fetcher, OptimisticGameMutationHelper } from "./query-cache-invalidation";

const DEFAULT_RETRY_POLICY: RetryPolicy = {
  maxAttempts: 3,
  initialBackoffMs: 500,
  backoffMultiplier: 2,
};

const DEFAULT_POLL_INTERVAL_MS = 2_000;
const DEFAULT_CONFIRMATION_TIMEOUT_MS = 30_000;

const ALLOWED_TRANSITIONS: Readonly<
  Record<TransactionPhase, ReadonlyArray<TransactionPhase>>
> = {
  [TransactionPhase.IDLE]: [TransactionPhase.VALIDATING],
  [TransactionPhase.VALIDATING]: [
    TransactionPhase.SUBMITTING,
    TransactionPhase.FAILED,
  ],
  [TransactionPhase.SUBMITTING]: [
    TransactionPhase.SUBMITTED,
    TransactionPhase.RETRYING,
    TransactionPhase.FAILED,
  ],
  [TransactionPhase.SUBMITTED]: [
    TransactionPhase.CONFIRMING,
    TransactionPhase.FAILED,
  ],
  [TransactionPhase.CONFIRMING]: [
    TransactionPhase.CONFIRMED,
    TransactionPhase.FAILED,
  ],
  [TransactionPhase.RETRYING]: [
    TransactionPhase.SUBMITTING,
    TransactionPhase.FAILED,
  ],
  [TransactionPhase.CONFIRMED]: [],
  [TransactionPhase.FAILED]: [],
};

interface TransactionOrchestratorOptions {
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  generateCorrelationId?: () => string;
}

export class TransactionOrchestrator {
  private state: TransactionOrchestratorState = {
    phase: TransactionPhase.IDLE,
    confirmations: 0,
    attempt: 0,
  };

  private readonly subscribers = new Set<TransactionStateSubscriber>();
  private readonly now: () => number;
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly generateCorrelationId: () => string;

  constructor(options: TransactionOrchestratorOptions = {}) {
    this.now = options.now ?? (() => Date.now());
    this.sleep =
      options.sleep ??
      ((ms: number) => new Promise((resolve) => setTimeout(resolve, ms)));
    this.generateCorrelationId =
      options.generateCorrelationId ??
      (() => `tx-${this.now()}-${Math.random().toString(16).slice(2, 10)}`);
  }

  getState<TData = unknown>(): TransactionOrchestratorState<TData> {
    return { ...(this.state as TransactionOrchestratorState<TData>) };
  }

  subscribe<TData = unknown>(
    subscriber: TransactionStateSubscriber<TData>,
  ): () => void {
    const wrapped: TransactionStateSubscriber =
      subscriber as TransactionStateSubscriber;
    this.subscribers.add(wrapped);
    wrapped(this.getState());
    return () => {
      this.subscribers.delete(wrapped);
    };
  }

  reset(): void {
    this.state = {
      phase: TransactionPhase.IDLE,
      confirmations: 0,
      attempt: 0,
    };
    this.notify();
  }

  async execute<TInput, TData>(
    request: TransactionRequest<TInput, TData>,
  ): Promise<TransactionResult<TData>> {
    if (!this.isIdleOrTerminal()) {
      const correlationId =
        this.state.correlationId ?? this.generateCorrelationId();
      const err = this.makeOrchestratorError(
        OrchestratorErrorCode.DUPLICATE_IN_FLIGHT,
        correlationId,
        {
          code: "API_VALIDATION_ERROR",
          domain: ErrorDomain.API,
          severity: ErrorSeverity.USER_ACTIONABLE,
          message: "Another transaction is already in progress.",
        },
      );
      return {
        success: false,
        correlationId,
        error: err,
        state: this.getState(),
      };
    }

    const startedAt = this.now();
    const correlationId = this.generateCorrelationId();

    this.state = {
      phase: TransactionPhase.IDLE,
      operation: request.operation,
      correlationId,
      confirmations: 0,
      attempt: 0,
      startedAt,
    };

    this.transition(TransactionPhase.VALIDATING, {});

    const preconditionError = request.validatePreconditions?.() ?? null;
    if (preconditionError) {
      return this.failWith(
        correlationId,
        OrchestratorErrorCode.PRECONDITION_FAILED,
        preconditionError,
      );
    }

    const inputError = request.validateInput?.(request.input) ?? null;
    if (inputError) {
      return this.failWith(
        correlationId,
        OrchestratorErrorCode.INVALID_INPUT,
        inputError,
      );
    }

    const retryPolicy = {
      ...DEFAULT_RETRY_POLICY,
      ...request.retryPolicy,
    };

    const submitResult = await this.submitWithRetry(
      request,
      retryPolicy,
      correlationId,
      startedAt,
    );
    if (!submitResult.success) {
      return submitResult;
    }

    this.transition(TransactionPhase.SUBMITTED, {
      txHash: submitResult.txHash,
      data: submitResult.data,
    });

    const confirmationResult = await this.confirmUntilSettled(
      request,
      submitResult.txHash,
      correlationId,
      startedAt,
      request.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS,
      request.confirmationTimeoutMs ?? DEFAULT_CONFIRMATION_TIMEOUT_MS,
    );

    if (!confirmationResult.success) {
      return confirmationResult;
    }

    this.transition(TransactionPhase.CONFIRMED, {
      confirmations: confirmationResult.confirmations,
      settledAt: this.now(),
    });

    return {
      success: true,
      correlationId,
      txHash: submitResult.txHash,
      data: submitResult.data,
      confirmations: confirmationResult.confirmations,
      state: this.getState<TData>(),
    };
  }

  private async submitWithRetry<TInput, TData>(
    request: TransactionRequest<TInput, TData>,
    retryPolicy: RetryPolicy,
    correlationId: string,
    startedAt: number,
  ): Promise<
    | { success: true; txHash: string; data: TData }
    | {
        success: false;
        correlationId: string;
        error: OrchestratorError;
        state: TransactionOrchestratorState<TData>;
      }
  > {
    let attempt = 0;

    while (attempt < retryPolicy.maxAttempts) {
      attempt += 1;

      this.transition(TransactionPhase.SUBMITTING, { attempt });

      const context: TransactionContext = {
        correlationId,
        operation: request.operation,
        attempt,
        startedAt,
      };

      try {
        const submission = await request.submit(request.input, context);
        if (
          typeof submission.txHash !== "string" ||
          submission.txHash.trim() === ""
        ) {
          return this.failWith(
            correlationId,
            OrchestratorErrorCode.SUBMISSION_FAILED,
            {
              code: "API_VALIDATION_ERROR",
              domain: ErrorDomain.API,
              severity: ErrorSeverity.TERMINAL,
              message: "Submission returned an empty transaction hash.",
            },
          );
        }

        return {
          success: true,
          txHash: submission.txHash.trim(),
          data: submission.data,
        };
      } catch (err) {
        const appError = this.normalizeError(err, {
          correlationId,
          operation: request.operation,
          phase: TransactionPhase.SUBMITTING,
          attempt,
        });

        if (
          appError.severity === ErrorSeverity.RETRYABLE &&
          attempt < retryPolicy.maxAttempts
        ) {
          this.transition(TransactionPhase.RETRYING, { attempt });
          const waitMs = this.computeBackoffMs(retryPolicy, attempt);
          await this.sleep(waitMs);
          continue;
        }

        return this.failWith(
          correlationId,
          OrchestratorErrorCode.SUBMISSION_FAILED,
          appError,
        );
      }
    }

    return this.failWith(
      correlationId,
      OrchestratorErrorCode.SUBMISSION_FAILED,
      {
        code: "UNKNOWN",
        domain: ErrorDomain.UNKNOWN,
        severity: ErrorSeverity.TERMINAL,
        message: "Submission retry budget exhausted.",
      },
    );
  }

  private async confirmUntilSettled<TInput, TData>(
    request: TransactionRequest<TInput, TData>,
    txHash: string,
    correlationId: string,
    startedAt: number,
    pollIntervalMs: number,
    timeoutMs: number,
  ): Promise<
    | { success: true; confirmations: number }
    | {
        success: false;
        correlationId: string;
        error: OrchestratorError;
        state: TransactionOrchestratorState<TData>;
      }
  > {
    this.transition(TransactionPhase.CONFIRMING, {});

    while (this.now() - startedAt <= timeoutMs) {
      const context: TransactionContext = {
        correlationId,
        operation: request.operation,
        attempt: this.state.attempt,
        startedAt,
      };

      try {
        const confirmation = await request.confirm(txHash, context);

        if (confirmation.status === ConfirmationStatus.CONFIRMED) {
          return {
            success: true,
            confirmations:
              confirmation.confirmations ?? this.state.confirmations,
          };
        }

        if (confirmation.status === ConfirmationStatus.FAILED) {
          const appError = confirmation.error ?? {
            code: "RPC_TX_REJECTED",
            domain: ErrorDomain.RPC,
            severity: ErrorSeverity.TERMINAL,
            message: "Transaction confirmation failed.",
          };
          return this.failWith(
            correlationId,
            OrchestratorErrorCode.CONFIRMATION_FAILED,
            appError,
          );
        }

        this.transition(TransactionPhase.CONFIRMING, {
          confirmations: confirmation.confirmations ?? this.state.confirmations,
        });
      } catch (err) {
        const appError = this.normalizeError(err, {
          correlationId,
          operation: request.operation,
          phase: TransactionPhase.CONFIRMING,
          txHash,
        });

        if (appError.severity !== ErrorSeverity.RETRYABLE) {
          return this.failWith(
            correlationId,
            OrchestratorErrorCode.CONFIRMATION_FAILED,
            appError,
          );
        }
      }

      await this.sleep(pollIntervalMs);
    }

    return this.failWith(correlationId, OrchestratorErrorCode.TIMEOUT, {
      code: "RPC_CONNECTION_TIMEOUT",
      domain: ErrorDomain.RPC,
      severity: ErrorSeverity.RETRYABLE,
      message: `Transaction ${txHash} was not confirmed within ${timeoutMs}ms.`,
    });
  }

  private failWith<TData>(
    correlationId: string,
    orchestratorCode: OrchestratorErrorCode,
    appError: AppError,
  ): {
    success: false;
    correlationId: string;
    error: OrchestratorError;
    state: TransactionOrchestratorState<TData>;
  } {
    const error = this.makeOrchestratorError(
      orchestratorCode,
      correlationId,
      appError,
    );
    this.transition(TransactionPhase.FAILED, {
      error,
      settledAt: this.now(),
    });

    return {
      success: false,
      correlationId,
      error,
      state: this.getState<TData>(),
    };
  }

  private transition(
    phase: TransactionPhase,
    patch: Partial<TransactionOrchestratorState>,
  ): void {
    const currentPhase = this.state.phase;
    if (
      phase !== currentPhase &&
      !ALLOWED_TRANSITIONS[currentPhase].includes(phase)
    ) {
      const correlationId =
        this.state.correlationId ?? this.generateCorrelationId();
      const error = this.makeOrchestratorError(
        OrchestratorErrorCode.INVALID_STATE,
        correlationId,
        {
          code: "UNKNOWN",
          domain: ErrorDomain.UNKNOWN,
          severity: ErrorSeverity.TERMINAL,
          message: `Invalid transaction phase transition: ${currentPhase} -> ${phase}`,
        },
      );

      this.state = {
        ...this.state,
        phase: TransactionPhase.FAILED,
        error,
        settledAt: this.now(),
      };
      this.notify();
      return;
    }

    this.state = {
      ...this.state,
      ...patch,
      phase,
      confirmations: patch.confirmations ?? this.state.confirmations,
      attempt: patch.attempt ?? this.state.attempt,
    };

    if (
      TERMINAL_TRANSACTION_PHASES.has(phase) &&
      this.state.settledAt === undefined
    ) {
      this.state = {
        ...this.state,
        settledAt: this.now(),
      };
    }

    this.notify();
  }

  private makeOrchestratorError(
    orchestratorCode: OrchestratorErrorCode,
    correlationId: string,
    appError: AppError,
  ): OrchestratorError {
    return {
      ...appError,
      orchestratorCode,
      correlationId,
      context: {
        ...(appError.context ?? {}),
        correlationId,
        phase: this.state.phase,
      },
    };
  }

  private computeBackoffMs(policy: RetryPolicy, attempt: number): number {
    const exponent = Math.max(0, attempt - 1);
    return Math.round(
      policy.initialBackoffMs * Math.pow(policy.backoffMultiplier, exponent),
    );
  }

  private normalizeError(
    err: unknown,
    context: Record<string, unknown>,
  ): AppError {
    if (this.isAppError(err)) {
      return {
        ...err,
        context: {
          ...(err.context ?? {}),
          ...context,
        },
      };
    }

    return toAppError(err, undefined, context);
  }

  private isAppError(value: unknown): value is AppError {
    return (
      typeof value === "object" &&
      value !== null &&
      "code" in value &&
      "domain" in value &&
      "severity" in value &&
      "message" in value
    );
  }

  private isIdleOrTerminal(): boolean {
    return (
      this.state.phase === TransactionPhase.IDLE ||
      TERMINAL_TRANSACTION_PHASES.has(this.state.phase)
    );
  }

  private notify(): void {
    const snapshot = this.getState();
    for (const subscriber of this.subscribers) {
      try {
        subscriber(snapshot);
      } catch {
        // Subscriber errors are intentionally isolated from orchestrator flow.
      }
    }
  }
}

/**
 * Run a transaction with optimistic cache apply before execute, revert on failure,
 * finalize on success (caller supplies final cache value or refetch).
 */
export async function executeGameActionWithOptimistic<
  TInput,
  TData,
  TOptimistic,
>(args: {
  orchestrator: TransactionOrchestrator;
  optimistic: OptimisticGameMutationHelper;
  cacheKey: QueryKey;
  optimisticData: TOptimistic;
  request: TransactionRequest<TInput, TData>;
  buildFinalize?: (success: TData) => TOptimistic | undefined;
  fetcher?: Fetcher<TOptimistic>;
}): Promise<ReturnType<TransactionOrchestrator["execute"]>> {
  const gen = args.optimistic.apply(args.cacheKey, args.optimisticData);
  const result = await args.orchestrator.execute(args.request);
  if (!result.success) {
    args.optimistic.revertIfLatest(args.cacheKey, gen);
    return result;
  }
  const data = result.data as TData;
  const finalSlice = args.buildFinalize?.(data);
  if (finalSlice !== undefined) {
    await args.optimistic.finalize(args.cacheKey, finalSlice);
  } else if (args.fetcher) {
    await args.optimistic.finalize(args.cacheKey, undefined, args.fetcher);
  } else {
    await args.optimistic.finalize(args.cacheKey, args.optimisticData as TOptimistic);
  }
  return result;
}

export default TransactionOrchestrator;
