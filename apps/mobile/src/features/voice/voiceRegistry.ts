// The voice sessions alive in this app, keyed by agent (docs/mobile/voice-mode-plan.md
// §1 "store keyed by agent"). A session's controller lives here from the first
// Voice screen open until End session, so it outlives navigation and
// backgrounding. `connectionManager` hands this the voice events and every
// reconnect; the screen opens and looks up controllers. Pure TypeScript on
// purpose: importing it costs nothing until a Voice screen opens (§2c).

import { EventKind, type HostEvent } from "../../protocol/gen/envelope_pb";
import { VoiceController, type VoiceControllerOptions } from "./VoiceController";
import { provisionFromProto, voiceStore, type VoiceStore } from "./voiceStore";

export type VoiceSessionOptions = Omit<VoiceControllerOptions, "store">;

export class VoiceRegistry {
  private readonly controllers = new Map<string, VoiceController>();

  constructor(private readonly store: VoiceStore, private readonly log?: (line: string) => void) {}

  /** The controller for `agentId`, created on the first open. A live session keeps its history. */
  open(options: VoiceSessionOptions): VoiceController {
    const existing = this.controllers.get(options.agentId);
    if (existing) {
      // The agent may have moved panes since the session began: type into where it is now.
      existing.retarget(options.paneId, options.sessionId);
      return existing;
    }
    const controller = new VoiceController({ ...options, store: this.store });
    this.controllers.set(options.agentId, controller);
    return controller;
  }

  get(agentId: string): VoiceController | undefined {
    return this.controllers.get(agentId);
  }

  /** End session: host registration cleared, file deleted, controller forgotten. */
  async end(agentId: string): Promise<void> {
    const controller = this.controllers.get(agentId);
    if (!controller) return;
    this.controllers.delete(agentId);
    await controller.endSession();
    // `voiceSession("")` clears every registration on the connection, not one
    // agent's (plan §4.3), so the sessions that remain register again.
    for (const survivor of this.controllers.values()) survivor.reregister();
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
        const controller = this.controllers.get(reply.agentId);
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
    for (const controller of this.controllers.values()) controller.onConnected();
  }

  /** The user disconnected or is dialling another host: every session is local-only now, so drop them. */
  disposeAll(): void {
    for (const controller of this.controllers.values()) controller.dispose();
    this.controllers.clear();
    this.store.getState().resetHostStatus();
  }
}

export const voiceRegistry = new VoiceRegistry(voiceStore);
