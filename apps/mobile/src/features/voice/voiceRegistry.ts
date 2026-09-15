// Voice sessions belong to tmux panes. A controller lives here from the first
// Voice screen open until End session, so navigation, reconnects, and root
// agent identity changes do not replace it.

import { EventKind, type HostEvent } from "../../protocol/gen/envelope_pb";
import { log } from "../../session/log";
import { VoiceController, type VoiceControllerOptions } from "./VoiceController";
import { VoiceRecorderCoordinator } from "./recorderCoordinator";
import { provisionFromProto, voiceStore, type VoiceStore } from "./voiceStore";

export type VoiceSessionOptions = Omit<VoiceControllerOptions, "recorderCoordinator" | "sessionKey" | "store">;

export class VoiceRegistry {
  private readonly controllers = new Map<string, VoiceController>();
  private readonly recorderCoordinator = new VoiceRecorderCoordinator();
  private serverIdentity: string | undefined;

  constructor(private readonly store: VoiceStore, private readonly log?: (line: string) => void) {}

  open(options: VoiceSessionOptions): VoiceController {
    this.selectServer(options.serverIdentity);
    const sessionKey = this.key(options.serverIdentity, options.paneId);
    const existing = this.controllers.get(sessionKey);
    if (existing) {
      existing.retarget(options.paneId, options.sessionId);
      return existing;
    }
    const controller = new VoiceController({
      ...options,
      sessionKey: options.paneId,
      recorderCoordinator: this.recorderCoordinator,
      store: this.store,
    });
    this.controllers.set(sessionKey, controller);
    return controller;
  }

  get(paneId: string): VoiceController | undefined {
    return this.serverIdentity === undefined
      ? undefined
      : this.controllers.get(this.key(this.serverIdentity, paneId));
  }

  async end(paneId: string): Promise<void> {
    if (this.serverIdentity === undefined) return;
    const sessionKey = this.key(this.serverIdentity, paneId);
    const controller = this.controllers.get(sessionKey);
    if (!controller) return;
    this.controllers.delete(sessionKey);
    await controller.endSession();
    // Clearing is connection-wide, so every surviving pane registers again.
    for (const survivor of this.controllers.values()) survivor.reregister();
  }

  onVoiceEvent(event: HostEvent): void {
    if (event.kind === EventKind.VOICE_PROVISION) {
      const progress = event.voice?.provision;
      if (progress) this.store.getState().applyProvisionProgress(provisionFromProto(progress));
      return;
    }
    if (event.kind !== EventKind.VOICE_REPLY) return;
    const reply = event.voice?.reply;
    if (!reply) return;
    const controller = this.controllers.get(this.key(reply.serverIdentity, reply.paneId));
    if (!controller) {
      this.log?.(`[muxflow] voice reply server=${reply.serverIdentity.slice(0, 12)} pane=${reply.paneId} ignored (no session)`);
      return;
    }
    controller.onVoiceReply(reply, event.voice?.status?.detail ?? "");
  }

  onConnected(serverIdentity: string): void {
    this.selectServer(serverIdentity);
    for (const controller of this.controllers.values()) controller.onConnected();
  }

  onServerChanged(serverIdentity: string): void {
    this.selectServer(serverIdentity);
  }

  disposeAll(): void {
    for (const controller of this.controllers.values()) controller.dispose();
    this.controllers.clear();
    this.serverIdentity = undefined;
    this.store.getState().resetHostStatus();
  }

  private selectServer(serverIdentity: string): void {
    if (this.serverIdentity === undefined) {
      this.serverIdentity = serverIdentity;
      return;
    }
    if (this.serverIdentity === serverIdentity) return;
    for (const controller of this.controllers.values()) controller.dispose();
    this.controllers.clear();
    this.serverIdentity = serverIdentity;
  }

  private key(serverIdentity: string, paneId: string): string {
    return `${serverIdentity}\u0000${paneId}`;
  }
}

export const voiceRegistry = new VoiceRegistry(voiceStore, log);
