// The voice sessions alive in this app, keyed locally for the controller's
// lifetime and indexed separately by current remote agent id. A controller lives here from the first
// Voice screen open until End session, so it outlives navigation and
// backgrounding. `connectionManager` hands this the voice events and every
// reconnect; the screen opens and looks up controllers. Pure TypeScript on
// purpose: importing it costs nothing until a Voice screen opens (§2c).

import { EventKind, type HostEvent } from "../../protocol/gen/envelope_pb";
import type { Agent } from "../../store/sessionStore";
import { log } from "../../session/log";
import { VoiceController, type VoiceControllerOptions } from "./VoiceController";
import { VoiceRecorderCoordinator } from "./recorderCoordinator";
import { provisionFromProto, voiceStore, type VoiceStore } from "./voiceStore";

export type VoiceSessionOptions = Omit<VoiceControllerOptions, "recorderCoordinator" | "sessionKey" | "store">;

export class VoiceRegistry {
  private readonly controllersBySessionKey = new Map<string, VoiceController>();
  private readonly sessionKeyByCurrentAgentId = new Map<string, string>();
  private readonly recorderCoordinator = new VoiceRecorderCoordinator();

  constructor(private readonly store: VoiceStore, private readonly log?: (line: string) => void) {}

  /** The controller for `agentId`, created on the first open. A live session keeps its history. */
  open(options: VoiceSessionOptions): VoiceController {
    const existingKey = this.sessionKeyByCurrentAgentId.get(options.agentId);
    const existing = existingKey ? this.controllersBySessionKey.get(existingKey) : undefined;
    if (existing) {
      // The agent may have moved panes since the session began: type into where it is now.
      existing.retarget(options.paneId, options.sessionId);
      return existing;
    }
    const sessionKey = this.availableSessionKey(options.agentId);
    const controller = new VoiceController({ ...options, sessionKey, recorderCoordinator: this.recorderCoordinator, store: this.store });
    this.controllersBySessionKey.set(controller.sessionKey, controller);
    this.sessionKeyByCurrentAgentId.set(options.agentId, controller.sessionKey);
    return controller;
  }

  get(agentId: string): VoiceController | undefined {
    const sessionKey = this.sessionKeyByCurrentAgentId.get(agentId);
    return sessionKey ? this.controllersBySessionKey.get(sessionKey) : undefined;
  }

  /**
   * Retarget one live controller selected only from ids explicitly retired by
   * the same authoritative event. Ambiguous conversations are left untouched.
   */
  promoteAgent(retiredAgentIds: readonly string[], agent: Agent): { oldAgentId: string } | undefined {
    const candidates = new Map<string, string>();
    for (const retiredId of retiredAgentIds) {
      const sessionKey = this.sessionKeyByCurrentAgentId.get(retiredId);
      if (sessionKey) candidates.set(sessionKey, retiredId);
    }
    if (candidates.size !== 1) {
      if (candidates.size > 1) this.log?.(`[muxflow] voice identity.promotion.rejected reason=multiple-sessions new=${agent.id} count=${candidates.size}`);
      return undefined;
    }
    const [sessionKey, oldAgentId] = candidates.entries().next().value!;
    const controller = this.controllersBySessionKey.get(sessionKey);
    const targetSessionKey = this.sessionKeyByCurrentAgentId.get(agent.id);
    if (!controller || (targetSessionKey !== undefined && targetSessionKey !== sessionKey)) {
      this.log?.(`[muxflow] voice identity.promotion.rejected reason=target-in-use new=${agent.id}`);
      return undefined;
    }
    for (const retiredId of retiredAgentIds) {
      if (this.sessionKeyByCurrentAgentId.get(retiredId) === sessionKey) {
        this.sessionKeyByCurrentAgentId.delete(retiredId);
      }
    }
    this.sessionKeyByCurrentAgentId.set(agent.id, sessionKey);
    controller.promoteAgent(agent);
    return { oldAgentId };
  }

  /** End session: host registration cleared, file deleted, controller forgotten. */
  async end(agentIdOrSessionKey: string): Promise<void> {
    const sessionKey = this.controllersBySessionKey.has(agentIdOrSessionKey)
      ? agentIdOrSessionKey
      : this.sessionKeyByCurrentAgentId.get(agentIdOrSessionKey);
    if (!sessionKey) return;
    const controller = this.controllersBySessionKey.get(sessionKey);
    if (!controller) return;
    this.controllersBySessionKey.delete(sessionKey);
    for (const [agentId, indexedSessionKey] of this.sessionKeyByCurrentAgentId) {
      if (indexedSessionKey === sessionKey) this.sessionKeyByCurrentAgentId.delete(agentId);
    }
    await controller.endSession();
    // `voiceSession("")` clears every registration on the connection, not one
    // agent's (plan §4.3), so the sessions that remain register again.
    for (const survivor of this.controllersBySessionKey.values()) survivor.reregister();
  }

  /** EVENT_KIND_VOICE_PROVISION → host status; EVENT_KIND_VOICE_REPLY → that agent's session. */
  onVoiceEvent(event: HostEvent): void {
    switch (event.kind) {
      case EventKind.VOICE_PROVISION: {
        const progress = event.voice?.provision;
        if (progress) this.store.getState().applyProvisionProgress(provisionFromProto(progress));
        return;
      }
      case EventKind.VOICE_REPLY: {
        const reply = event.voice?.reply;
        if (!reply) return;
        const sessionKey = this.sessionKeyByCurrentAgentId.get(reply.agentId);
        const controller = sessionKey ? this.controllersBySessionKey.get(sessionKey) : undefined;
        if (!controller) {
          this.log?.(`[muxflow] voice reply for ${reply.agentId} ignored (no session)`);
          return;
        }
        controller.onVoiceReply(reply, event.voice?.status?.detail ?? "");
        return;
      }
      default:
        return;
    }
  }

  /** Every reconnect: registrations are per connection (§4.3). */
  onConnected(): void {
    for (const controller of this.controllersBySessionKey.values()) controller.onConnected();
  }

  /** The user disconnected or is dialling another host: every session is local-only now, so drop them. */
  disposeAll(): void {
    for (const controller of this.controllersBySessionKey.values()) controller.dispose();
    this.controllersBySessionKey.clear();
    this.sessionKeyByCurrentAgentId.clear();
    this.store.getState().resetHostStatus();
  }

  /** Initial IDs are readable keys; a live retired key collision gets a bounded local suffix. */
  private availableSessionKey(initialAgentId: string): string {
    if (!this.controllersBySessionKey.has(initialAgentId)) return initialAgentId;
    let suffix = 2;
    while (this.controllersBySessionKey.has(`${initialAgentId}:${suffix}`)) suffix += 1;
    return `${initialAgentId}:${suffix}`;
  }
}

export const voiceRegistry = new VoiceRegistry(voiceStore, log);
