import type { ConnectionSpec, HostProfile } from "../../app/types";

/**
 * The connection a saved host is opened with. An SSH profile saved before the
 * connection carried its own id is given the profile's, so
 * `hostProfileId(connection)` names the profile it was saved under.
 */
export function profileConnection(profile: HostProfile): ConnectionSpec {
  return profile.connection.mode === "ssh"
    ? { ...profile.connection, profileId: profile.connection.profileId || profile.id }
    : profile.connection;
}

/** The host mark drawn before a host's rows: its letter, else its label's first character. */
export function hostLetter(profile: Pick<HostProfile, "id" | "label" | "letter">): string {
  const [letter] = profile.letter ?? "";
  const [fromLabel] = profile.label.trim();
  const [fromId] = profile.id;
  return (letter ?? fromLabel ?? fromId ?? "").toUpperCase();
}

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
