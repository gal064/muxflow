import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { fileDesc, messageDesc } from "@bufbuild/protobuf/codegenv2";
import { describe, expect, it } from "vitest";
import { validateHostContract } from "./contract";
import { EnvelopeSchema, type Envelope } from "./gen/envelope_pb";
import released from "./testing/released-v0.1.9.json";
import { hostEnvelope, serverHello } from "./testing/fakeTransport";

// Actual generated descriptor from the tagged mobile/host release. This stays
// test-only: runtime admission has no version branches or legacy message paths.
const releasedEnvelope = messageDesc(fileDesc(released.fileDescriptor), released.envelopeMessageIndex);
// Harmless length-delimited envelope field 2047 ("new"), absent from both schemas.
const unknownField = new Uint8Array([0xfa, 0x7f, 3, 110, 101, 119]);
const additive = (bytes: Uint8Array) => new Uint8Array([...bytes, ...unknownField]);

describe(`admission against ${released.release} (${released.commit.slice(0, 7)})`, () => {
  it.each([released.protocolMajor - 1, released.protocolMajor + 1])("refuses major %s in either update direction", (major) => {
    expect(validateHostContract(major)).toMatchObject({ kind: "protocolMajor", advertised: major });
  });

  it("the released mobile decoder ignores a harmless additive host field under the same major", () => {
    const frame = hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 9n }) }, { requestId: 1n });
    const oldApp = fromBinary(releasedEnvelope, additive(toBinary(EnvelopeSchema, frame))) as Envelope;
    expect(oldApp.protocolMajor).toBe(released.protocolMajor);
    expect(oldApp.payload.case).toBe("serverHello");
    if (oldApp.payload.case !== "serverHello") throw new Error("missing hello");
    expect(oldApp.payload.value).toMatchObject({ serverIdentity: "server-a", connectionEpoch: 9n });
    expect(validateHostContract(oldApp.protocolMajor)).toBeUndefined();
  });

  it("the released host schema ignores a harmless new app field and its old response still decodes", () => {
    const hello = create(EnvelopeSchema, {
      protocolMajor: released.protocolMajor, requestId: 1n,
      payload: { case: "clientHello", value: { connectionEpoch: 12n } },
    });
    const oldHost = fromBinary(releasedEnvelope, additive(toBinary(EnvelopeSchema, hello))) as Envelope;
    expect(oldHost.payload.case).toBe("clientHello");
    if (oldHost.payload.case !== "clientHello") throw new Error("missing client hello");
    expect(oldHost.payload.value.connectionEpoch).toBe(12n);
    const oldResponse = fromBinary(releasedEnvelope, toBinary(EnvelopeSchema, hostEnvelope({ case: "serverHello", value: serverHello({ connectionEpoch: 12n }) }, { requestId: 1n })));
    const newApp = fromBinary(EnvelopeSchema, toBinary(releasedEnvelope, oldResponse));
    expect(validateHostContract(newApp.protocolMajor)).toBeUndefined();
    expect(newApp.payload.case).toBe("serverHello");
    if (newApp.payload.case !== "serverHello") throw new Error("missing server hello");
    expect(newApp.payload.value.connectionEpoch).toBe(12n);
  });
});
