import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { fileDesc, messageDesc } from "@bufbuild/protobuf/codegenv2";
import { describe, expect, it } from "vitest";
import { PROTOCOL_MAJOR, validateHostContract } from "./contract";
import { EnvelopeSchema, type Envelope } from "./gen/envelope_pb";
import released from "./testing/released-v0.1.9.json";
import { hostEnvelope, serverHello } from "./testing/fakeTransport";

// Actual generated descriptor from the tagged mobile/host release. This stays
// test-only: runtime admission has no version branches or legacy message paths.
const releasedEnvelope = messageDesc(fileDesc(released.fileDescriptor), released.envelopeMessageIndex);
// Harmless length-delimited envelope field 2047 ("new"), absent from both schemas.
const unknownField = new Uint8Array([0xfa, 0x7f, 3, 110, 101, 119]);
const additive = (bytes: Uint8Array) => new Uint8Array([...bytes, ...unknownField]);

// Chat mode raised the major past this release's: a released phone cannot show
// a chat pane, and this app's chat requests mean nothing to a released host.
// The two refuse each other, so what has to keep working across the bump is
// the refusal itself: each side still reads the other's hello and its major.
// Once a release with this major is recorded here, the same-major checks come
// back against it: an additive field is tolerated in both directions.
describe(`admission against ${released.release} (${released.commit.slice(0, 7)})`, () => {
  it.each([released.protocolMajor, PROTOCOL_MAJOR + 1])("refuses major %s in either update direction", (major) => {
    expect(major).not.toBe(PROTOCOL_MAJOR);
    expect(validateHostContract(major)).toMatchObject({ kind: "protocolMajor", advertised: major });
  });

  it("the released mobile decoder reads this host's hello and its newer major, past an additive field", () => {
    const frame = hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 9n }) }, { requestId: 1n });
    const oldApp = fromBinary(releasedEnvelope, additive(toBinary(EnvelopeSchema, frame))) as Envelope;
    expect(oldApp.protocolMajor).toBe(PROTOCOL_MAJOR);
    expect(oldApp.protocolMajor).not.toBe(released.protocolMajor);
    expect(oldApp.payload.case).toBe("serverHello");
    if (oldApp.payload.case !== "serverHello") throw new Error("missing hello");
    expect(oldApp.payload.value).toMatchObject({ serverIdentity: "server-a", connectionEpoch: 9n });
  });

  it("the released host schema reads this app's hello and major, and this app reads and refuses its answer", () => {
    const hello = create(EnvelopeSchema, {
      protocolMajor: PROTOCOL_MAJOR, requestId: 1n,
      payload: { case: "clientHello", value: { connectionEpoch: 12n } },
    });
    const oldHost = fromBinary(releasedEnvelope, additive(toBinary(EnvelopeSchema, hello))) as Envelope;
    expect(oldHost.protocolMajor).toBe(PROTOCOL_MAJOR);
    expect(oldHost.payload.case).toBe("clientHello");
    if (oldHost.payload.case !== "clientHello") throw new Error("missing client hello");
    expect(oldHost.payload.value.connectionEpoch).toBe(12n);
    const answer = create(EnvelopeSchema, {
      protocolMajor: released.protocolMajor, requestId: 1n,
      payload: { case: "serverHello", value: serverHello({ connectionEpoch: 12n }) },
    });
    const oldResponse = fromBinary(releasedEnvelope, toBinary(EnvelopeSchema, answer));
    const newApp = fromBinary(EnvelopeSchema, toBinary(releasedEnvelope, oldResponse));
    expect(validateHostContract(newApp.protocolMajor)).toMatchObject({ kind: "protocolMajor", advertised: released.protocolMajor });
    expect(newApp.payload.case).toBe("serverHello");
    if (newApp.payload.case !== "serverHello") throw new Error("missing server hello");
    expect(newApp.payload.value.connectionEpoch).toBe(12n);
  });
});
