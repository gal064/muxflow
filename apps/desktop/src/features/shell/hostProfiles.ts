import type { HostProfile } from "../../app/types";

export function savedSshProfileId(target: string, configPath: string): string {
  const slug = target.replace(/[^a-zA-Z0-9_-]/g, "-").replace(/-+/g, "-").slice(0, 64) || "host";
  const bytes = new TextEncoder().encode(`${target}\0${configPath}`);
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) {
    hash ^= BigInt(byte);
    hash = BigInt.asUintN(64, hash * 0x100000001b3n);
  }
  return `ssh-${slug}-${hash.toString(16).padStart(16, "0")}`;
}

export function profileIdForSshConnection(
  profiles: readonly HostProfile[],
  target: string,
  configPath: string,
): string {
  return profiles.find((profile) => profile.connection.mode === "ssh"
    && profile.connection.target === target
    && (profile.connection.configPath ?? "") === configPath)?.id
    ?? savedSshProfileId(target, configPath);
}
