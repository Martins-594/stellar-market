import type { Prisma } from "@prisma/client";

interface DlqEntry {
  id: number;
  cursor: string;
  payload: unknown;
  error: string;
  attempt: number;
  replayedAt: Date | null;
}

const mockState = {
  cursor: "0",
  cursorUpdates: [] as string[],
  lastIndexedLedger: null as number | null,
  dlq: [] as DlqEntry[],
  jobFindFirstFailure: null as Error | null,
  lockHeld: false,
  jobs: {} as Record<string, any>,
  txQueue: Promise.resolve(),
};

const mockGetEvents = jest.fn();
const mockGetLatestLedger = jest.fn();

const mockCursorUpsert = jest.fn(
  async ({ update, create }: Prisma.HorizonCursorUpsertArgs) => {
    const next = update.cursor ?? create.cursor;
    mockState.cursor = next as string;
    mockState.cursorUpdates.push(next as string);
    return { id: 1, cursor: mockState.cursor, updatedAt: new Date() };
  },
);

const mockJobFindFirst = jest.fn(async () => {
  if (mockState.jobFindFirstFailure) throw mockState.jobFindFirstFailure;
  const jobs = Object.values(mockState.jobs);
  return jobs.length > 0 ? jobs[0] : null;
});

const mockRedisSet = jest.fn();
const mockRedisDel = jest.fn();
const mockRedisClient = {
  set: mockRedisSet,
  del: mockRedisDel,
  on: jest.fn(),
  quit: jest.fn(),
};

jest.mock("ioredis", () => jest.fn(() => mockRedisClient));

const mockPrisma = {
  $transaction: jest.fn(async (fn: any) => {
    const result = mockState.txQueue.then(() => fn(mockPrisma));
    mockState.txQueue = result;
    return result;
  }),
  horizonCursor: {
    upsert: mockCursorUpsert,
    findUnique: jest.fn(async () => ({ id: 1, cursor: mockState.cursor })),
  },
  syncState: {
    upsert: jest.fn(async ({ update }: Prisma.SyncStateUpsertArgs) => {
      mockState.lastIndexedLedger = update.lastIndexedLedger as number;
      return { id: "default", lastIndexedLedger: mockState.lastIndexedLedger };
    }),
  },
  horizonDlq: {
    create: jest.fn(async ({ data }: Prisma.HorizonDlqCreateArgs) => {
      const entry: DlqEntry = {
        id: mockState.dlq.length + 1,
        replayedAt: null,
        ...(data as unknown as Omit<DlqEntry, "id" | "replayedAt">),
      };
      mockState.dlq.push(entry);
      return entry;
    }),
    count: jest.fn(async () => mockState.dlq.filter((entry) => !entry.replayedAt).length),
    findMany: jest.fn(async () => mockState.dlq),
    update: jest.fn(),
  },
  job: {
    findFirst: mockJobFindFirst,
    findUnique: jest.fn(async ({ where }: { where: { id: string } }) => {
      return mockState.jobs[where.id] ?? null;
    }),
    update: jest.fn(async ({ where, data }: { where: { id: string }; data: any }) => {
      const job = mockState.jobs[where.id];
      if (!job) throw new Error("Job not found");
      Object.assign(job, data);
      return job;
    }),
    updateMany: jest.fn().mockResolvedValue({ count: 0 }),
  },
};

jest.mock("@prisma/client", () => ({
  PrismaClient: jest.fn(() => mockPrisma),
  BadgeTier: {
    BRONZE: "BRONZE",
    SILVER: "SILVER",
    GOLD: "GOLD",
    PLATINUM: "PLATINUM",
  },
  EscrowEventType: {
    JOB_CREATED: "JOB_CREATED",
    JOB_FUNDED: "JOB_FUNDED",
    PAYMENT_RELEASED: "PAYMENT_RELEASED",
    DISPUTE_OPENED: "DISPUTE_OPENED",
    DISPUTE_RESOLVED: "DISPUTE_RESOLVED",
    REFUNDED: "REFUNDED",
    EXPIRED: "EXPIRED",
  },
  NotificationType: {
    PAYMENT_RELEASED: "PAYMENT_RELEASED",
    DISPUTE_RAISED: "DISPUTE_RAISED",
    DISPUTE_RESOLVED: "DISPUTE_RESOLVED",
    BADGE_AWARDED: "BADGE_AWARDED",
  },
}));

jest.mock("@stellar/stellar-sdk", () => ({
  rpc: {
    Server: jest.fn(() => ({
      getEvents: mockGetEvents,
      getLatestLedger: mockGetLatestLedger,
    })),
  },
  scValToNative: jest.fn((value: { native: unknown }) => value.native),
  xdr: {
    ScVal: {
      fromXDR: jest.fn(),
    },
  },
}));

jest.mock("../../config", () => ({
  config: {
    stellar: {
      rpcUrl: "https://rpc.test",
      escrowContractId: "escrow-contract",
      disputeContractId: "dispute-contract",
      reputationContractId: "reputation-contract",
    },
  },
}));

jest.mock("../notification.service", () => ({
  NotificationService: { deliverPersistedNotification: jest.fn() },
}));

jest.mock("../../lib/logger", () => ({
  logger: { info: jest.fn(), warn: jest.fn(), error: jest.fn(), debug: jest.fn() },
}));

function makeEvent(
  pagingToken: string,
  topic: unknown[] = ["unknown", "unknown"],
  value: unknown[] = [],
) {
  const scVal = (native: unknown) => ({
    native,
    toXDR: jest.fn(() => Buffer.from(JSON.stringify(native)).toString("base64")),
  });

  return {
    id: pagingToken,
    pagingToken,
    type: "contract",
    ledger: Number(pagingToken),
    ledgerClosedAt: "2026-06-19T12:00:00Z",
    contractId: "contract",
    txHash: `tx-${pagingToken}`,
    inSuccessfulContractCall: true,
    topic: topic.map(scVal),
    value: scVal(value),
  };
}

describe("durable Horizon listener", () => {
  beforeEach(() => {
    jest.clearAllMocks();
    mockState.cursor = "0";
    mockState.cursorUpdates = [];
    mockState.lastIndexedLedger = null;
    mockState.dlq = [];
    mockState.jobFindFirstFailure = null;
    mockState.lockHeld = false;
    mockState.jobs = {};
    mockState.txQueue = Promise.resolve();

    mockRedisSet.mockImplementation(() => {
      if (mockState.lockHeld) return null;
      mockState.lockHeld = true;
      return "OK";
    });
    mockRedisDel.mockImplementation(() => {
      mockState.lockHeld = false;
      return 1;
    });
  });

  it("resumes from the persisted cursor after a simulated restart without gaps or duplicates", async () => {
    // Simulate a listener that already completed its initial bootstrap poll in
    // an earlier process, persisting cursor "100" — this poll should paginate
    // forward from there rather than replaying from ledger zero.
    mockState.cursor = "100";
    mockGetLatestLedger.mockResolvedValue({ sequence: 1_000 });

    const events = Array.from({ length: 10 }, (_, index) => makeEvent(String(101 + index)));
    mockGetEvents.mockImplementation(
      async ({ cursor }: { cursor: string; limit: number }) => {
        const offset = Number(cursor) - 100;
        return { events: events.slice(offset, offset + 5) };
      },
    );

    let service = await import("../horizon-listener.service");
    await service.pollHorizonOnce();

    jest.resetModules();
    service = await import("../horizon-listener.service");
    await service.pollHorizonOnce();

    expect(mockGetEvents.mock.calls.map(([request]) => request.cursor)).toEqual([
      "100",
      "105",
    ]);
    expect(mockState.cursorUpdates).toEqual(["105", "110"]);
    expect(mockState.cursor).toBe("110");
    expect(mockState.lastIndexedLedger).toBe(110);
  });

  it("moves a three-time failure to the DLQ", async () => {
    mockState.jobFindFirstFailure = new Error("database read failed");
    const service = await import("../horizon-listener.service");

    await service.processHorizonEvent(
      makeEvent("42", ["escrow", "created"], [42]) as unknown as Parameters<
        typeof service.processHorizonEvent
      >[0],
    );

    expect(mockJobFindFirst).toHaveBeenCalledTimes(3);
    expect(mockState.dlq).toHaveLength(1);
    expect(mockState.dlq[0]).toEqual(
      expect.objectContaining({
        cursor: "42",
        error: "database read failed",
        attempt: 1,
      }),
    );
  }, 10_000);

  it("acquires a distributed lock before polling and does not poll while another instance holds it", async () => {
    const service = await import("../horizon-listener.service");

    mockState.lockHeld = true;

    await service.pollHorizonOnce();

    expect(mockGetEvents).not.toHaveBeenCalled();
    expect(mockRedisSet).toHaveBeenCalledWith(
      expect.any(String),
      expect.any(String),
      "EX",
      expect.any(Number),
      "NX",
    );
  });

  it("releases the distributed lock after polling so another instance can take over", async () => {
    const service = await import("../horizon-listener.service");

    mockGetEvents.mockResolvedValue({ events: [] });
    mockGetLatestLedger.mockResolvedValue({ sequence: 1 });

    await service.pollHorizonOnce();

    expect(mockGetEvents).toHaveBeenCalled();
    expect(mockRedisDel).toHaveBeenCalled();
    expect(mockState.lockHeld).toBe(false);
  });

  it("expires a stale lock after a crash so another instance can take over", async () => {
    const service = await import("../horizon-listener.service");

    mockGetEvents.mockResolvedValue({ events: [] });
    mockGetLatestLedger.mockResolvedValue({ sequence: 1 });

    // Simulate a crash: acquire the lock but never release it.
    mockRedisDel.mockImplementationOnce(() => {});
    await service.pollHorizonOnce();

    expect(mockState.lockHeld).toBe(true);

    // Lock TTL expires, allowing another instance to acquire it.
    mockState.lockHeld = false;
    mockRedisSet.mockClear();
    mockRedisDel.mockClear();
    mockGetEvents.mockClear();

    await service.pollHorizonOnce();

    expect(mockGetEvents).toHaveBeenCalled();
    expect(mockState.lockHeld).toBe(false);
  });

  it("applies escrow events atomically for the same job and prevents state regression under concurrency", async () => {
    const jobId = "job-1";
    mockState.jobs[jobId] = { id: jobId, status: "CREATED", lastEventId: 0 };

    const service = await import("../escrow-projection.service");

    const event1 = makeEvent("1", ["escrow", "DISPUTE_OPENED"], [jobId]);
    const event2 = makeEvent("2", ["escrow", "DISPUTE_RESOLVED"], [jobId]);

    // Sequential result is the source of truth
    await service.handleEscrowEvent(event1);
    await service.handleEscrowEvent(event2);
    const expectedStatus = mockState.jobs[jobId].status;
    expect(expectedStatus).not.toBe("CREATED");

    // Reset for concurrent run
    mockState.jobs[jobId] = { id: jobId, status: "CREATED", lastEventId: 0 };
    mockState.txQueue = Promise.resolve();
    (mockPrisma.$transaction as jest.Mock).mockClear();

    await Promise.all([
      service.handleEscrowEvent(event1),
      service.handleEscrowEvent(event2),
    ]);

    expect(mockState.jobs[jobId].status).toBe(expectedStatus);
    expect(mockPrisma.$transaction).toHaveBeenCalled();
  });
});
