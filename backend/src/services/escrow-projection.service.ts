import { PrismaClient, Prisma, EscrowEvent, EscrowEventType, JobStatus, EscrowStatus } from "@prisma/client";
import { NotificationService } from "./notification.service";
import { FraudDetectionService } from "./fraud-detection.service";
import { logger } from "../lib/logger";

const prisma = new PrismaClient();

export interface EscrowProjection {
  status: JobStatus;
  escrowStatus: EscrowStatus;
}

export const initialState: EscrowProjection = {
  status: "OPEN",
  escrowStatus: "UNFUNDED",
};

export function applyEvent(state: EscrowProjection, event: EscrowEvent): EscrowProjection {
  switch (event.eventType) {
    case EscrowEventType.JOB_CREATED:
      return {
        ...state,
        escrowStatus: "UNFUNDED",
      };
    case EscrowEventType.JOB_FUNDED:
      return {
        ...state,
        escrowStatus: "FUNDED",
        status: "IN_PROGRESS",
      };
    case EscrowEventType.PAYMENT_RELEASED:
      return {
        ...state,
        escrowStatus: "COMPLETED",
        status: "COMPLETED",
      };
    case EscrowEventType.DISPUTE_OPENED:
      return {
        ...state,
        escrowStatus: "DISPUTED",
        status: "DISPUTED",
      };
    case EscrowEventType.DISPUTE_RESOLVED: {
      const payload = event.payload as Record<string, unknown>;
      const rawStatus = payload?.rawStatus;
      let jobStatus = state.status;
      let escrowStatus = state.escrowStatus;

      if (rawStatus === "ResolvedForClient") {
        jobStatus = "CANCELLED";
        escrowStatus = "CANCELLED";
      } else if (rawStatus === "ResolvedForFreelancer") {
        jobStatus = "COMPLETED";
        escrowStatus = "COMPLETED";
      } else if (rawStatus === "RefundedBoth") {
        jobStatus = "CANCELLED";
        escrowStatus = "CANCELLED";
      }
      return {
        ...state,
        status: jobStatus,
        escrowStatus: escrowStatus,
      };
    }
    case EscrowEventType.REFUNDED:
      return {
        ...state,
        escrowStatus: "CANCELLED",
        status: "CANCELLED",
      };
    case EscrowEventType.EXPIRED:
      return {
        ...state,
        escrowStatus: "CANCELLED",
        status: "EXPIRED",
      };
    default:
      return state;
  }
}

export async function projectJobState(
  jobId: string,
  tx?: Prisma.TransactionClient
): Promise<EscrowProjection> {
  const client = tx ?? prisma;
  const events = await client.escrowEvent.findMany({
    where: { jobId },
    orderBy: { ledgerSeq: "asc" },
  });

  return events.reduce((state, event) => applyEvent(state, event), initialState);
}

export interface HandleEscrowEventInput {
  jobId: string;
  contractJobId: string;
  eventType: EscrowEventType;
  ledgerSeq: number;
  txHash: string;
  payload: Record<string, unknown>;
}

export async function handleEscrowEvent(eventData: HandleEscrowEventInput): Promise<void> {
  const { jobId, contractJobId, eventType, ledgerSeq, txHash, payload } = eventData;

  const result = await prisma.$transaction(async (tx) => {
    // Lock the job row before reading/projecting so concurrent event handling
    // for the same job serializes instead of producing a lost update.
    await tx.$queryRaw`SELECT id FROM "Job" WHERE id = ${jobId} FOR UPDATE`;

    try {
      // Attempt insert — silently skip if duplicate
      await tx.escrowEvent.create({
        data: {
          jobId,
          contractJobId,
          eventType,
          ledgerSeq,
          txHash,
          payload: (payload ?? {}) as Prisma.InputJsonValue,
        },
      });
    } catch (error) {
      // Check for unique constraint violation (idempotency key match). Duck-typed
      // rather than `instanceof Prisma.PrismaClientKnownRequestError` so this also
      // recognizes equivalent error shapes from test doubles/other DB drivers.
      const isUniqueConstraintViolation =
        typeof error === "object" && error !== null && "code" in error && error.code === "P2002";
      if (isUniqueConstraintViolation) {
        logger.info(
          { contractJobId, eventType, ledgerSeq },
          "[EscrowProjectionService] Duplicate event ignored"
        );
        return null;
      }
      throw error;
    }

    // Fetch current Job state before updating
    const previousState = await tx.job.findUnique({
      where: { id: jobId },
      select: { status: true, escrowStatus: true },
    });

    // Re-project current state from the complete event log
    const nextState = await projectJobState(jobId, tx);

    // Materialize projected state back into the Job table
    await tx.job.update({
      where: { id: jobId },
      data: nextState,
    });

    return { previousState, nextState };
  });

  if (!result) {
    return;
  }

  const { previousState, nextState } = result;

  // Execute event-specific side-effects and notifications
  const stateChanged =
    !previousState ||
    previousState.status !== nextState.status ||
    previousState.escrowStatus !== nextState.escrowStatus;

  if (eventType === EscrowEventType.PAYMENT_RELEASED) {
    if (stateChanged || previousState?.status !== "COMPLETED") {
      const job = await prisma.job.findUnique({
        where: { id: jobId },
        select: { clientId: true, freelancerId: true, title: true, contractJobId: true },
      });
      if (job) {
        const notifyIds = [job.clientId, job.freelancerId].filter(Boolean) as string[];
        await Promise.all(
          notifyIds.map((userId) =>
            NotificationService.sendNotification({
              userId,
              type: "PAYMENT_RELEASED",
              title: "Payment Released",
              message: `All payments for "${job.title}" have been released on-chain.`,
              metadata: { contractJobId: job.contractJobId ?? contractJobId },
              skipBatching: true,
            })
          )
        );

        // Near-real-time fraud/anomaly scoring (issue #900). Fire-and-forget:
        // scoring an escrow release must never block or fail projection.
        FraudDetectionService.onEscrowReleased(
          jobId,
          [job.clientId, job.freelancerId].filter(Boolean) as string[],
        );
      }
    }
  } else if (eventType === EscrowEventType.DISPUTE_OPENED) {
    const onChainDisputeId = payload?.onChainDisputeId;
    if (typeof onChainDisputeId === "string") {
      const job = await prisma.job.findUnique({
        where: { id: jobId },
        select: { clientId: true, freelancerId: true, contractJobId: true },
      });
      if (job) {
        await prisma.dispute.upsert({
          where: { onChainDisputeId },
          update: { status: "OPEN" },
          create: {
            jobId,
            onChainDisputeId,
            clientId: job.clientId,
            freelancerId: job.freelancerId ?? job.clientId,
            initiatorId: job.clientId,
            reason: "Raised on-chain",
            status: "OPEN",
          },
        });

        if (stateChanged || previousState?.status !== "DISPUTED") {
          const notifyIds = [job.clientId, job.freelancerId].filter(Boolean) as string[];
          await Promise.all(
            notifyIds.map((userId) =>
              NotificationService.sendNotification({
                userId,
                type: "DISPUTE_RAISED",
                title: "Dispute Opened",
                message: "A dispute has been opened on-chain for your job.",
                metadata: { onChainDisputeId, contractJobId: job.contractJobId ?? contractJobId },
              })
            )
          );
        }
      }
    }
  } else if (eventType === EscrowEventType.DISPUTE_RESOLVED) {
    const onChainDisputeId = payload?.onChainDisputeId;
    const rawStatus = payload?.rawStatus;
    if (typeof onChainDisputeId === "string" && typeof rawStatus === "string") {
      let dbDisputeStatus: "OPEN" | "IN_PROGRESS" | "RESOLVED" = "RESOLVED";
      let outcome: string = rawStatus;

      if (rawStatus === "ResolvedForClient") {
        outcome = "CLIENT_WINS";
      } else if (rawStatus === "ResolvedForFreelancer") {
        outcome = "FREELANCER_WINS";
      } else if (rawStatus === "RefundedBoth") {
        outcome = "REFUND_BOTH";
      } else if (rawStatus === "Escalated") {
        dbDisputeStatus = "IN_PROGRESS";
        outcome = "ESCALATED";
      }

      const dispute = await prisma.dispute.findUnique({
        where: { onChainDisputeId },
        select: { id: true, clientId: true, freelancerId: true, status: true },
      });

      if (dispute) {
        await prisma.dispute.update({
          where: { id: dispute.id },
          data: {
            status: dbDisputeStatus,
            outcome,
            resolvedAt: dbDisputeStatus === "RESOLVED" ? new Date() : null,
          },
        });

        if (stateChanged || dispute.status !== dbDisputeStatus) {
          const notifyIds = [dispute.clientId, dispute.freelancerId].filter(Boolean) as string[];
          await Promise.all(
            notifyIds.map((userId) =>
              NotificationService.sendNotification({
                userId,
                type: "DISPUTE_RESOLVED",
                title: "Dispute Resolved",
                message: `The dispute has been resolved on-chain: ${outcome}.`,
                metadata: { onChainDisputeId, outcome },
              })
            )
          );
        }
      }
    }
  }
}
