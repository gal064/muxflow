// Fakes for the voice tests: a scripted connection, an in-memory recorder,
// player and file store. Test-only.

import { create, type MessageInitShape } from "@bufbuild/protobuf";
import type { HostConnection } from "../../protocol/HostConnection";
import { Operation, ResponseSchema, VoiceResponseSchema, VoiceSpeechSchema, VoiceStatusSchema, VoiceTranscriptSchema, type Request, type Response, type VoiceReadiness } from "../../protocol/gen/envelope_pb";
import type { PlayerStatus, VoiceFiles, VoicePlayer, VoiceRecorder } from "./audioPorts";

export type Answer = (request: Request) => Response | Promise<Response>;

export class FakeConnection {
  state: "connected" | "reconnecting" = "connected";
  readonly requests: Request[] = [];
  private readonly answers = new Map<Operation, Answer>();

  answer(operation: Operation, answer: Answer): void {
    this.answers.set(operation, answer);
  }

  request(request: Request): Promise<Response> {
    this.requests.push(request);
    const answer = this.answers.get(request.operation);
    if (!answer) return Promise.resolve(create(ResponseSchema, { ok: true }));
    try {
      return Promise.resolve(answer(request));
    } catch (error) {
      return Promise.reject(error);
    }
  }

  /** Requests of one operation, oldest first. */
  of(operation: Operation): Request[] {
    return this.requests.filter((request) => request.operation === operation);
  }

  asHostConnection(): HostConnection {
    return this as unknown as HostConnection;
  }
}

export function statusResponse(readiness: VoiceReadiness, overrides: MessageInitShape<typeof VoiceStatusSchema> = {}): Response {
  return create(ResponseSchema, {
    ok: true,
    voice: create(VoiceResponseSchema, { status: create(VoiceStatusSchema, { readiness, modelDownloadBytes: 671088640n, ...overrides }) }),
  });
}

export function transcriptResponse(text: string): Response {
  return create(ResponseSchema, { ok: true, voice: create(VoiceResponseSchema, { transcript: create(VoiceTranscriptSchema, { text, audioMillis: 1200, decodeMillis: 80 }) }) });
}

export function speechResponse(audio: Uint8Array, text: string): Response {
  return create(ResponseSchema, { ok: true, voice: create(VoiceResponseSchema, { speech: create(VoiceSpeechSchema, { audio, audioMime: "audio/mpeg", text }) }) });
}

export class FakeRecorder implements VoiceRecorder {
  prepared = 0;
  recording = false;
  nextUri: string | null = "file:///cache/rec-1.m4a";
  nextDurationMs = 2_000;
  released = 0;
  async prepare(): Promise<void> {
    this.prepared += 1;
  }
  record(): void {
    if (this.prepared === 0) throw new Error("recorder not prepared");
    this.recording = true;
  }
  async stop(): Promise<{ uri: string | null; durationMs: number }> {
    if (!this.recording) return { uri: null, durationMs: 0 };
    this.recording = false;
    return { uri: this.nextUri, durationMs: this.nextDurationMs };
  }
  release(): void {
    // Like the adapter: a live utterance is not released from under the controller, and releasing nothing is a no-op.
    if (this.recording || this.prepared === 0) return;
    this.released += 1;
    this.prepared = 0;
  }
  state(): "unprepared" | "prepared" | "recording" {
    return this.recording ? "recording" : this.prepared > 0 ? "prepared" : "unprepared";
  }
}

export class FakePlayer implements VoicePlayer {
  loaded: string | undefined;
  playing = false;
  positionMs = 0;
  readonly calls: string[] = [];
  private readonly listeners = new Set<(status: PlayerStatus) => void>();
  load(uri: string): void {
    this.loaded = uri;
    this.positionMs = 0;
    this.calls.push(`load ${uri}`);
  }
  play(): void {
    this.playing = true;
    this.calls.push("play");
  }
  pause(): void {
    this.playing = false;
    this.calls.push("pause");
  }
  stop(): void {
    this.playing = false;
    this.positionMs = 0;
    this.calls.push("stop");
  }
  seek(positionMs: number): void {
    this.positionMs = positionMs;
    this.calls.push(`seek ${positionMs}`);
  }
  rate = 1;
  setRate(rate: number): void {
    this.rate = rate;
    this.calls.push(`rate ${rate}`);
  }
  onStatus(listener: (status: PlayerStatus) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
  release(): void {}
  /** The native player reporting. */
  emit(status: Partial<PlayerStatus>): void {
    const full: PlayerStatus = { positionMs: this.positionMs, durationMs: 4_000, playing: this.playing, finished: false, ...status };
    for (const listener of this.listeners) listener(full);
  }
}

export class FakeFiles implements VoiceFiles {
  readonly files = new Map<string, Uint8Array>();
  readonly deleted: string[] = [];
  constructor() {
    this.files.set("file:///cache/rec-1.m4a", new TextEncoder().encode("aac-bytes"));
  }
  async read(uri: string): Promise<Uint8Array> {
    const bytes = this.files.get(uri);
    if (!bytes) throw new Error(`no such file ${uri}`);
    return bytes;
  }
  writeReply(agentId: string, bytes: Uint8Array): string {
    const uri = `file:///cache/voice/${agentId}.mp3`;
    this.files.set(uri, bytes);
    return uri;
  }
  delete(uri: string): void {
    this.deleted.push(uri);
    this.files.delete(uri);
  }
}
