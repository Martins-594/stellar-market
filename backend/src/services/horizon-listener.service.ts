import { rpc, scValToNative } from "@stellar/stellar-sdk";
import { randomUUID } from "node:crypto";
import { PrismaClient, BadgeTier, EscrowEventType } from "@prisma/client";
import Redis from "ioredis";
import { config } from "../config";
import { NotificationService } from "./notification.service";
import { logger } from "../lib/logger";
import { CircuitBreaker } from "../lib/circuit-breaker";
import type { CircuitBreakerStatus } from "../lib/circuit-breaker";
import { handleEscrowEvent } from "./escrow-projection.service";
import { ReputationCacheService } from "./reputation-cache.service";

export type { CircuitBreakerStatus };
export type { CircuitState } from "../lib/circuit-breaker";

const prisma = new PrismaClient();
const server = new rpc.Server(config.stellar.rpcUrl);

const POLL_INTERVAL_MS = 5_000;
const MAX_EVENTS_PER_POLL = 200;
const SYNC_STATE_ID = "default";
const CURSOR_ID = 1;
const MAX_EVENT_RETRIES = 3;

const HORIZON_LISTENER_LOCK_KEY = "horizon-listener:lock";
const HORIZON_LISTENER_LOCK_TTL_MS = 30_000;
const HORIZON_LISTENER_LOCK_RENEW_INTERVAL_MS = HORIZON_LISTENER_LOCK_TTL_MS / 3;
const redis = new Redis(config.redisUrl);

async function releaseHorizonListenerLock(token: string): Promise<void> {
  await redis.eval(
    "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end",
    1,
    HORIZON_LISTENER_LOCK_KEY,
    token,
  );
}

async function acquireHorizonListenerLock(): Promise<(() => Promise<void>) | null> {
  const token = randomUUID();
  const acquired = await redis.set(
    HORIZON_LISTENER_LOCK_KEY,
    token,
    "PX",
    HORIZON_LISTENER_LOCK_TTL_MS,
    "NX",
  );
  if (acquired !== "OK") return null;

  const renewal = setInterval(() => {
    redis
      .eval(
        "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('pexpire', KEYS[1], ARGV[2]) else return 0 end",
        1,
        HORIZON_LISTENER_LOCK_KEY,
        token,
        HORIZON_LISTENER_LOCK_TTL_MS,
      )
      .catch((err: unknown) => {
        logger.warn({ err }, "[HorizonListener] Failed to renew horizon listener lock");
      });
  }, HORIZON_LISTENER_LOCK_RENEW_INTERVAL_MS);
  renewal.unref();

  return async () => {
    clearInterval(renewal);
    await releaseHorizonListenerLock(token);
  };
}

// ─── Circuit Breaker instance ─────────────────────────────────────────────────

const horizonCB = new CircuitBreaker({
  failureThreshold: 5,
  openDurationMs: 60_000,
  name: "HorizonListener",
});

/** Derive the health string exposed on GET /health. */
export function getHorizonListenerHealth(): "connected" | "degraded" | "down" {
  return horizonCB.getHealthLabel();
}

/** Full circuit-breaker status (for tests / internal use). */
export function getCircuitBreakerStatus(): Readonly<CircuitBreakerStatus> {
  return horizonCB.getStatus();
}

// ─── Soroban event types ──────────────────────────────────────────────────────

type SorobanEvent = Awaited<ReturnType<typeof server.getEvents>>["events"][number];

// ─── helpers ──────────────────────────────────────────────────────────────────

function topicToStrings(event: SorobanEvent): string[] {
  return event.topic.map((t) => String(scValToNative(t) ?? ""));
}

/** Handles both plain-string and single-element-array Soroban enum variants. */
function enumVariant(raw: unknown): string {
  if (typeof raw === "string") return raw;
  if (Array.isArray(raw) && raw.length > 0) return String(raw[0]);
  return String(raw ?? "");
}

function bigintToStr(v: unknown): string {
  return typeof v === "bigint" ? v.toString() : String(v ?? "");
}

function toBadgeTier(raw: unknown): BadgeTier | null {
  const v = enumVariant(raw).toUpperCase();
  if (v === "BRONZE") return BadgeTier.BRONZE;
  if (v === "SILVER") return BadgeTier.SILVER;
  if (v === "GOLD") return BadgeTier.GOLD;
  if (v === "PLATINUM") return BadgeTier.PLATINUM;
  return null;
}

// ─── sync-state persistence ───────────────────────────────────────────────────

async function getLastIndexedLedger(): Promise<number> {
  const row = await prisma.syncState.upsert({
    where: { id: SYNC_STATE_ID },
    update: {},
    create: { id: SYNC_STATE_ID, lastIndexedLedger: 0 },
  });
  return row.lastIndexedLedger;
}

async function setLastIndexedLedger(ledger: number): Promise<void> {
  await prisma.syncState.upsert({
    where: { id: SYNC_STATE_ID },
    update: { lastIndexedLedger: ledger },
    create: { id: SYNC_STATE_ID, lastIndexedLedger: ledger },
  });
}

// ─── cursor persistence (paging token) ───────────────────────────────────────

async function getPersistedCursor(): Promise<string | null> {
  const row = await prisma.horizonCursor.findUnique({ where: { id: CURSOR_ID } });
  return row?.cursor ?? null;
}

async function saveCursor(cursor: string): Promise<void> {
  await prisma.horizonCursor.upsert({
    where: { id: CURSOR_ID },
    update: { cursor },
    create: { id: CURSOR_ID, cursor },
  });
}

// ─── event handlers ───────────────────────────────────────────────────────────

/**
 * escrow / created — (job_count: u64, client: Address, freelancer: Address)
 */
async function handleJobCreated(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 1) return;

  const onChainJobId = bigintToStr(data[0]);

  const job = await prisma.job.findFirst({
    where: { contractJobId: onChainJobId },
    select: { id: true },
  });

  if (!job) {
    logger.warn({ contractJobId: onChainJobId }, "[HorizonListener] JobCreated — no DB job");
    return;
  }

  await handleEscrowEvent({
    jobId: job.id,
    contractJobId: onChainJobId,
    eventType: EscrowEventType.JOB_CREATED,
    ledgerSeq: event.ledger,
    txHash: event.txHash,
    payload: {},
  });

  logger.info({ contractJobId: onChainJobId }, "[HorizonListener] JobCreated");
}

/**
 * escrow / funded — (job_id: u64, client: Address)
 */
async function handleJobFunded(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 1) return;

  const onChainJobId = bigintToStr(data[0]);

  const job = await prisma.job.findFirst({
    where: { contractJobId: onChainJobId },
    select: { id: true },
  });

  if (!job) {
    logger.warn({ contractJobId: onChainJobId }, "[HorizonListener] JobFunded — no DB job");
    return;
  }

  await handleEscrowEvent({
    jobId: job.id,
    contractJobId: onChainJobId,
    eventType: EscrowEventType.JOB_FUNDED,
    ledgerSeq: event.ledger,
    txHash: event.txHash,
    payload: {},
  });

  logger.info({ contractJobId: onChainJobId }, "[HorizonListener] JobFunded");
}

/**
 * escrow / pmt_released — (job_id: u64, freelancer: Address, amount: i128)
 */
async function handlePaymentReleased(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 1) return;

  const onChainJobId = bigintToStr(data[0]);
  const amount = data.length >= 3 ? bigintToStr(data[2]) : "0";

  const job = await prisma.job.findFirst({
    where: { contractJobId: onChainJobId },
    select: { id: true },
  });

  if (!job) {
    logger.warn({ contractJobId: onChainJobId }, "[HorizonListener] PaymentReleased — no DB job");
    return;
  }

  await handleEscrowEvent({
    jobId: job.id,
    contractJobId: onChainJobId,
    eventType: EscrowEventType.PAYMENT_RELEASED,
    ledgerSeq: event.ledger,
    txHash: event.txHash,
    payload: { amount },
  });

  logger.info({ contractJobId: onChainJobId }, "[HorizonListener] PaymentReleased");
}

/**
 * dispute / raised — (dispute_id: u64, job_id: u64, initiator: Address)
 */
async function handleDisputeOpened(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 3) return;

  const onChainDisputeId = bigintToStr(data[0]);
  const onChainJobId = bigintToStr(data[1]);

  const job = await prisma.job.findFirst({
    where: { contractJobId: onChainJobId },
    select: { id: true },
  });

  if (!job) {
    logger.warn({ contractJobId: onChainJobId }, "[HorizonListener] DisputeOpened — no DB job");
    return;
  }

  await handleEscrowEvent({
    jobId: job.id,
    contractJobId: onChainJobId,
    eventType: EscrowEventType.DISPUTE_OPENED,
    ledgerSeq: event.ledger,
    txHash: event.txHash,
    payload: { onChainDisputeId },
  });

  logger.info({ onChainDisputeId }, "[HorizonListener] DisputeOpened");
}

/**
 * dispute / resolved — (dispute_id: u64, dispute_status: DisputeStatus)
 */
async function handleDisputeResolved(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 2) return;

  const onChainDisputeId = bigintToStr(data[0]);
  const rawStatus = enumVariant(data[1]);

  const dispute = await prisma.dispute.findUnique({
    where: { onChainDisputeId },
    select: { 
      jobId: true, 
      job: { 
        select: { 
          contractJobId: true,
          client: { select: { walletAddress: true } },
          freelancer: { select: { walletAddress: true } },
        } 
      } 
    },
  });

  if (!dispute) {
    logger.warn({ onChainDisputeId }, "[HorizonListener] DisputeResolved — no DB dispute");
    return;
  }

  await handleEscrowEvent({
    jobId: dispute.jobId,
    contractJobId: dispute.job.contractJobId ?? "",
    eventType: EscrowEventType.DISPUTE_RESOLVED,
    ledgerSeq: event.ledger,
    txHash: event.txHash,
    payload: { onChainDisputeId, rawStatus },
  });

  // Invalidate reputation cache for both client and freelancer (dispute affects reputation)
  if (dispute.job.client?.walletAddress) {
    await ReputationCacheService.invalidateCache(dispute.job.client.walletAddress);
  }
  if (dispute.job.freelancer?.walletAddress) {
    await ReputationCacheService.invalidateCache(dispute.job.freelancer.walletAddress);
  }

  logger.info(
    { onChainDisputeId, rawStatus }, 
    "[HorizonListener] DisputeResolved - caches invalidated"
  );
}

/**
 * reput / badge — (user_address: Address, tier: ReputationTier)
 */
async function handleBadgeAwarded(event: SorobanEvent): Promise<void> {
  const data = scValToNative(event.value) as unknown[];
  if (!Array.isArray(data) || data.length < 2) return;

  const walletAddress = String(data[0] ?? "");
  const tier = toBadgeTier(data[1]);

  if (!walletAddress || !tier) return;

  const user = await prisma.user.findUnique({
    where: { walletAddress },
    select: { id: true },
  });

  if (!user) {
    logger.warn({ walletAddress }, "[HorizonListener] BadgeAwarded — no user");
    return;
  }

  const result = await prisma.badge.upsert({
    where: { userId_tier: { userId: user.id, tier } },
    update: {},
    create: {
      userId: user.id,
      tier,
      awardedLedger: event.ledger,
    },
  });

  if (result.awardedLedger === event.ledger) {
    await NotificationService.sendNotification({
      userId: user.id,
      type: "BADGE_AWARDED",
      title: `${tier.charAt(0) + tier.slice(1).toLowerCase()} Badge Earned!`,
      message: `Congratulations! You earned a ${tier.toLowerCase()} reputation badge on-chain.`,
      metadata: { tier, awardedLedger: event.ledger },
      skipBatching: true,
    });
  }

  // Invalidate reputation cache for this user
  await ReputationCacheService.invalidateCache(walletAddress);
  logger.info({ walletAddress, tier }, "[HorizonListener] BadgeAwarded - cache invalidated");
}

// ─── event dispatch ───────────────────────────────────────────────────────────

async function resolvePreRegisteredTx(txHash: string, ledger: number): Promise<void> {
  try {
    await prisma.transaction.updateMany({
      where: { txHash, status: "PENDING" },
      data: { status: "SUCCESS", confirmedLedger: ledger },
    });
  } catch (err) {
    logger.warn({ err, txHash }, "[HorizonListener] Failed to resolve pre-registered tx");
  }
}

async function dispatchEvent(event: SorobanEvent): Promise<void> {
  const [contract, name] = topicToStrings(event);

  if (contract === "escrow") {
    if (name === "created") return await handleJobCreated(event);
    if (name === "funded") return await handleJobFunded(event);
    if (name === "pmt_released") return await handlePaymentReleased(event);
  }

  if (contract === "dispute") {
    if (name === "raised") return await handleDisputeOpened(event);
    if (name === "resolved") return await handleDisputeResolved(event);
  }

  if (contract === "reput") {
    if (name === "badge") return await handleBadgeAwarded(event);
  }
}

async function processEvent(event: SorobanEvent): Promise<void> {
  // Promote any PENDING pre-registration for this txHash to SUCCESS immediately.
  await resolvePreRegisteredTx(event.txHash, event.ledger);

  let lastErr: unknown;
  for (let attempt = 0; attempt < MAX_EVENT_RETRIES; attempt++) {
    try {
      await dispatchEvent(event);
      return;
    } catch (err) {
      lastErr = err;
      logger.warn(
        { err, ledger: event.ledger, attempt: attempt + 1 },
        "[HorizonListener] Event processing failed, retrying",
      );
    }
  }

  // All retries exhausted — write to DLQ
  const errorMessage = lastErr instanceof Error ? lastErr.message : String(lastErr);
  logger.error(
    { ledger: event.ledger, cursor: event.pagingToken, error: errorMessage },
    "[HorizonListener] Event moved to DLQ after max retries",
  );
  try {
    await prisma.horizonDlq.create({
      data: {
        cursor: event.pagingToken,
        payload: Buffer.from(JSON.stringify(event)).toString("base64"),
        error: errorMessage,
        attempt: 1,
      },
    });
  } catch (dlqErr) {
    logger.error({ dlqErr }, "[HorizonListener] Failed to write event to DLQ");
  }
}

/** @internal Exported for testing only. */
export const processHorizonEvent = processEvent;

// ─── polling loop (circuit-breaker guarded) ───────────────────────────────────

async function poll(): Promise<void> {
  // Circuit breaker gate
  if (!horizonCB.allowRequest()) {
    const status = horizonCB.getStatus();
    logger.debug(
      { state: status.state, openedAt: status.openedAt },
      "[HorizonListener] Circuit open — skipping poll",
    );
    return;
  }

  let release: (() => Promise<void>) | null;
  try {
    release = await acquireHorizonListenerLock();
  } catch (err) {
    logger.error({ err }, "[HorizonListener] Failed to acquire horizon listener lock");
    return;
  }

  if (!release) {
    logger.debug("[HorizonListener] Horizon listener lock not acquired — skipping poll");
    return;
  }

  try {
    await pollWithLock();
  } finally {
    await release();
  }
}

async function pollWithLock(): Promise<void> {
  const contractIds = [
    config.stellar.escrowContractId,
    config.stellar.disputeContractId,
    config.stellar.reputationContractId,
  ].filter(Boolean);

  if (contractIds.length === 0) {
    return;
  }

  const lastLedger = await getLastIndexedLedger();
  const persistedCursor = await getPersistedCursor();

  let events: SorobanEvent[] = [];
  let maxEventLedger = lastLedger;

  try {
    if (persistedCursor !== null) {
      // Resume from persisted paging cursor
      const result = await server.getEvents({
        cursor: persistedCursor,
        limit: MAX_EVENTS_PER_POLL,
        filters: [{ type: "contract", contractIds }],
      });
      events = result.events;
      horizonCB.onSuccess();
    } else {
      // Bootstrap: use startLedger
      let startLedger: number;
      try {
        const latest = await server.getLatestLedger();
        if (lastLedger === 0) {
          startLedger = latest.sequence;
          await setLastIndexedLedger(startLedger);
          logger.info({ startLedger }, "[HorizonListener] First run — starting from ledger");
          horizonCB.onSuccess();
          return;
        }
        startLedger = lastLedger + 1;
        if (startLedger > latest.sequence) {
          horizonCB.onSuccess();
          return;
        }
      } catch (err) {
        logger.error({ err }, "[HorizonListener] Failed to fetch latest ledger");
        horizonCB.onFailure();
        return;
      }

      const result = await server.getEvents({
        startLedger,
        filters: [{ type: "contract", contractIds }],
        limit: MAX_EVENTS_PER_POLL,
      });
      events = result.events;
      horizonCB.onSuccess();
    }
  } catch (err) {
    const msg: string = err instanceof Error ? err.message : "";
    if (msg.includes("startLedger") || msg.includes("ledger") || msg.includes("cursor")) {
      logger.warn("[HorizonListener] Cursor/ledger out of retention window, resetting");
      try {
        const latest = await server.getLatestLedger();
        await setLastIndexedLedger(latest.sequence);
        horizonCB.onSuccess();
      } catch {
        horizonCB.onFailure();
      }
    } else {
      logger.error({ err }, "[HorizonListener] getEvents error");
      horizonCB.onFailure();
    }
    return;
  }

  for (const event of events) {
    await processEvent(event);
    if (event.ledger > maxEventLedger) maxEventLedger = event.ledger;
  }

  // Persist paging cursor from last event for next poll
  if (events.length > 0) {
    const lastEvent = events[events.length - 1];
    if (lastEvent.pagingToken) {
      await saveCursor(lastEvent.pagingToken);
    }
    if (typeof lastEvent.ledger === "number") {
      await setLastIndexedLedger(lastEvent.ledger);
    }
  } else if (maxEventLedger > lastLedger) {
    await setLastIndexedLedger(maxEventLedger);
  }
}

// ─── public API ───────────────────────────────────────────────────────────────

/** @internal Exported for testing only. */
export const pollHorizonOnce = poll;

/**
 * @internal Exported for testing only.
 * Returns the reconnect delay in milliseconds for the given consecutive failure count.
 * Starts at 1s, doubles each attempt, caps at 60s. Returns 0 on no failures.
 */
export function computeReconnectBackoffMs(failures: number): number {
  if (failures === 0) return 0;
  return Math.min(1_000 * Math.pow(2, failures - 1), 60_000);
}

let intervalId: NodeJS.Timeout | null = null;

export function startHorizonListener(): void {
  if (intervalId) return;

  const contractIds = [
    config.stellar.escrowContractId,
    config.stellar.disputeContractId,
    config.stellar.reputationContractId,
  ].filter(Boolean);

  if (contractIds.length === 0) {
    logger.info("[HorizonListener] No contract IDs configured — skipping");
    return;
  }

  logger.info(
    { intervalSeconds: POLL_INTERVAL_MS / 1_000 },
    "[HorizonListener] Starting",
  );
  logger.info({ contractIds }, "[HorizonListener] Watching contracts");

  const runPoll = async () => {
    try {
      await poll();
    } catch (err) {
      logger.error({ err }, "[HorizonListener] Poll error");
    }
  };

  void runPoll();
  intervalId = setInterval(() => void runPoll(), POLL_INTERVAL_MS);
}

export function stopHorizonListener(): void {
  if (intervalId) {
    clearInterval(intervalId);
    intervalId = null;
    logger.info("[HorizonListener] Stopped");
  }
}
export async function getHorizonStatus(): Promise<{ cursor: string; dlqDepth: number; lastEventTimestamp: Date | null }> {
  const syncState = await prisma.syncState.upsert({
    where: { id: SYNC_STATE_ID },
    update: {},
    create: { id: SYNC_STATE_ID, lastIndexedLedger: 0 },
  });
  const dlqDepth = await prisma.horizonDlq.count({ where: { replayedAt: null } });
  return { cursor: syncState.lastIndexedLedger.toString(), dlqDepth, lastEventTimestamp: syncState.updatedAt || null };
}

export async function overrideHorizonCursor(cursor: string): Promise<void> {
  const ledger = parseInt(cursor, 10);
  if (!isNaN(ledger)) {
    await prisma.syncState.upsert({
      where: { id: SYNC_STATE_ID },
      update: { lastIndexedLedger: ledger },
      create: { id: SYNC_STATE_ID, lastIndexedLedger: ledger },
    });
  }
}

export async function replayHorizonDlq(): Promise<{ replayed: number; failed: number }> {
  // Fetch all DLQ entries that have not yet been successfully replayed.
  const pending = await prisma.horizonDlq.findMany({
    where: { replayedAt: null },
    orderBy: { id: "asc" },
  });

  let replayed = 0;
  let failed = 0;

  for (const entry of pending) {
    let event: SorobanEvent;
    try {
      // Payload is stored as a base64-encoded JSON string of the original event.
      const raw =
        typeof entry.payload === "string"
          ? entry.payload
          : Buffer.from(JSON.stringify(entry.payload)).toString();
      const jsonStr = Buffer.from(raw, "base64").toString("utf8");
      event = JSON.parse(jsonStr) as SorobanEvent;
    } catch (parseErr) {
      logger.error(
        { id: entry.id, err: parseErr },
        "[HorizonListener] DLQ replay — failed to deserialise payload, skipping",
      );
      await prisma.horizonDlq.update({
        where: { id: entry.id },
        data: {
          attempt: { increment: 1 },
          error: `Payload deserialisation error: ${parseErr instanceof Error ? parseErr.message : String(parseErr)}`,
        },
      });
      failed++;
      continue;
    }

    try {
      await processEvent(event);
      await prisma.horizonDlq.update({
        where: { id: entry.id },
        data: { replayedAt: new Date() },
      });
      replayed++;
      logger.info(
        { id: entry.id, cursor: entry.cursor },
        "[HorizonListener] DLQ entry replayed successfully",
      );
    } catch (replayErr) {
      const errorMessage =
        replayErr instanceof Error ? replayErr.message : String(replayErr);
      await prisma.horizonDlq.update({
        where: { id: entry.id },
        data: {
          attempt: { increment: 1 },
          error: errorMessage,
        },
      });
      failed++;
      logger.error(
        { id: entry.id, cursor: entry.cursor, err: replayErr },
        "[HorizonListener] DLQ entry replay failed",
      );
    }
  }

  logger.info(
    { replayed, failed, total: pending.length },
    "[HorizonListener] DLQ replay complete",
  );
  return { replayed, failed };
}
