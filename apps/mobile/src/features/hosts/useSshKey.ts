// This phone's SSH key (design.md §6.2, §9.2, §9.9). The private half never
// leaves the native module; this only ever holds the public line.

import { useCallback, useEffect, useState } from "react";

import { log } from "./logBuffer";
import { muxflowSsh } from "../../ssh/MuxflowSsh";

export interface SshKeyHandle {
  /** `undefined` while loading, `null` when this phone has no key yet. */
  publicKey: string | null | undefined;
  busy: boolean;
  error: string | null;
  /** §6.2: generating when a key exists replaces it — the UI confirms first. */
  generate(): void;
}

export function useSshKey(): SshKeyHandle {
  const [publicKey, setPublicKey] = useState<string | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    muxflowSsh()
      .getPublicKey()
      .then(
        (key) => {
          if (!cancelled) setPublicKey(key);
        },
        (failure: unknown) => {
          if (cancelled) return;
          log(`key.read.failed ${describe(failure)}`);
          setPublicKey(null);
          setError("This phone's SSH key could not be read.");
        },
      );
    return () => {
      cancelled = true;
    };
  }, []);

  const generate = useCallback(() => {
    setBusy(true);
    setError(null);
    muxflowSsh()
      .generateKeyPair()
      .then(
        (result) => {
          setPublicKey(result.publicKeyOpenSsh);
          setBusy(false);
        },
        (failure: unknown) => {
          log(`key.generate.failed ${describe(failure)}`);
          setError("The key could not be generated on this phone.");
          setBusy(false);
        },
      );
  }, []);

  return { publicKey, busy, error, generate };
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
