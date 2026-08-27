import { rpc, scValToNative } from "@stellar/stellar-sdk";
import { PrismaClient, BadgeTier, EscrowEventType } from "@prisma/client";
import { config } from "../config";
import { NotificationService } from "./notification.service";
import { logger } from "../lib/logger";
import { CircuitBreaker } from "../lib/circuit-breaker";
import type { CircuitBreakerStatus } from "../lib/circuit-breaker";
import { handleEscrowEvent } from "./escrow-projection.service";
import { ReputationCacheService } from "./reputation-cache.service";
import { randomUUID } from "crypto";
import Redis from "ioredis";

export type { CircuitBreakerStatus };
export type { CircuitState } from "../lib/circuit-breaker";

const prisma = new PrismaClient();
const server = new rpc.Server(config.stellar.rpcUrl);

const POLL_INTERVAL_MS = 5_000;
const MAX_EVENTS_PER_POLL = 200;
const SYNC_STATE_ID = "default";
const CURSOR_ID = 1;
const MAX_EVENT_RETRIES = 3;

// Distributed lock configuration
const LOCK_KEY = "horizon-listener:lock";
const LOCK_TTL_MS = 60_000;
const LOCK_RENEW_INTERVAL_MS = 20_000;
let lockValue: string | null = null;
let lockRenewInterval: NodeJS.Timeout | null = null;
let isShuttingDown = false;

const redis = new Redis((config as any).redis?.url ?? process.env.REDIS_URL ?? "redis://127.0.0.1:6379");

// Circuit Breaker instance
const horizonCB = new CircuitBreaker({
  failureThreshold: 5,
  openDurationMs: 60_000,
  name: "HorizonListener",
});

export function getHorizonListenerHealth(): "connected" | "degraded" | "down" {
  return horizonCB.getHealthLabel();
}

export function getCircuitBreakerStatus(): Readonly<CircuitBreakerStatus> {
  return horizonCB.getStatus();
}

type SorobanEvent = Awaited<ReturnType<typeof server.getEvents>>["events"][number];

function topicToStrings(event: SorobanEvent): string[] {
  return event.topic.map((t) => String(scValToNative(t) ?? ""));
}

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
        },
      },
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

  await ReputationCacheService.invalidateCache(walletAddress);
  logger.info({ walletAddress, tier }, "[HorizonListener] BadgeAwarded - cache invalidated");
}

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

  const errorMessage = lastErr instanceof Error ? lastErr.message : String(lastErr);
  logger.error(
    { ledger: event.ledger, cursor: event.pagingToken, error: errorMessage },
    "[HorizonListener] Event processing failed after retries, moving to DLQ"
  );
}

// Distributed lock helpers
async function acquireLock(): Promise<boolean> {
  if (lockValue) return true;
  lockValue = randomUUID();
  const result = await redis.set(LOCK_KEY, lockValue, "PX", LOCK_TTL_MS, "NX");
  if (result === "OK") {
    startLockRenewal();
    return true;
  }
  lockValue = null;
  return false;
}

async function releaseLock(): Promise<void> {
  if (!lockValue) return;
  stopLockRenewal();
  const script = `
    if redis.call("get", KEYS[1]) == ARGV[1] then
      return redis.call("del", KEYS[1])
    else
      return 0
    end
  `;
  await redis.eval(script, 1, LOCK_KEY, lockValue);
  lockValue = null;
}

function startLockRenewal(): void {
  stopLockRenewal();
  lockRenewInterval = setInterval(async () => {
    if (lockValue) {
      try {
        await redis.pexpire(LOCK_KEY, LOCK_TTL_MS);
      } catch (err) {
        logger.warn({ err }, "[HorizonListener] Failed to renew lock");
      }
    }
  }, LOCK_RENEW_INTERVAL_MS);
}

function stopLockRenewal(): void {
  if (lockRenewInterval) {
    clearInterval(lockRenewInterval);
    lockRenewInterval = null;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function pollAndProcessEvents(): Promise<void> {
  const lastIndexedLedger = await getLastIndexedLedger();
  const cursor = await getPersistedCursor();

  const eventsResponse = await server.getEvents({
    startLedger: lastIndexedLedger + 1,
    cursor: cursor ?? undefined,
    limit: MAX_EVENTS_PER_POLL,
  });

  const events = eventsResponse.events;
  if (events.length === 0) {
    return;
  }

  for (const event of events) {
    await processEvent(event);
  }

  const lastEvent = events[events.length - 1];
  await saveCursor(lastEvent.pagingToken);
  await setLastIndexedLedger(lastEvent.ledger);

  logger.info({ count: events.length }, "[HorizonListener] Processed batch of events");
}

export async function startHorizonListener(): Promise<void> {
  logger.info("[HorizonListener] Starting horizon listener");

  const shutdown = async () => {
    isShuttingDown = true;
    await releaseLock();
    process.exit(0);
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);

  while (!isShuttingDown) {
    const acquired = await acquireLock();
    if (!acquired) {
      await sleep(POLL_INTERVAL_MS);
      continue;
    }

    try {
      await pollAndProcessEvents();
      horizonCB.recordSuccess();
    } catch (err) {
      logger.error({ err }, "[HorizonListener] Poll error");
      horizonCB.recordFailure();
    } finally {
      await releaseLock();
    }

    await sleep(POLL_INTERVAL_MS);
  }
}
