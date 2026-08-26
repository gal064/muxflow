// §13 step 7's `data`, encoded for the native bridge and decoded on tap.
//
// The bridge carries JSON, which has no bigint, so `attentionGeneration`
// travels as a decimal string. Decoding is defensive: the same tap listener
// sees every notification the OS hands the app, including the foreground
// service's ongoing one (§6.3), which carries no payload at all.

import type { NotificationToPost } from "./decide";
import type { NotificationPayload } from "./host";

export interface TapTarget {
  agentId: string;
  paneId: string;
  sessionId: string;
  attentionGeneration: bigint;
}

export function encodePayload(data: NotificationToPost["data"]): NotificationPayload {
  return {
    agentId: data.agentId,
    paneId: data.paneId,
    sessionId: data.sessionId,
    attentionGeneration: data.attentionGeneration.toString(),
  };
}

export function decodePayload(data: unknown): TapTarget | undefined {
  if (typeof data !== "object" || data === null) return undefined;
  const record = data as Record<string, unknown>;
  const agentId = stringField(record.agentId);
  const paneId = stringField(record.paneId);
  if (agentId === undefined || paneId === undefined) return undefined;
  const generation = generationField(record.attentionGeneration);
  if (generation === undefined) return undefined;
  return { agentId, paneId, sessionId: stringField(record.sessionId) ?? "", attentionGeneration: generation };
}

function stringField(value: unknown): string | undefined {
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function generationField(value: unknown): bigint | undefined {
  // Android hands the value back as the string it was sent as; a re-serialising
  // OS that turned it into a number would still be readable.
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return BigInt(value);
  if (typeof value !== "string" || !/^\d+$/.test(value)) return undefined;
  return BigInt(value);
}
