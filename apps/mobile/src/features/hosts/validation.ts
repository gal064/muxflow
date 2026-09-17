// Validation for the Add / edit host form (design.md §9.2). Pure: the screen
// renders what this returns and disables `Save` while `valid` is false.

import { DEFAULT_SSH_PORT, type HostDraft } from "../../store/hostsStore";

export interface HostFormValues {
  host: string;
  /** Kept as typed so the field can show what the user entered. */
  port: string;
  user: string;
  label: string;
}

export type HostFormField = "host" | "port" | "user";

export type HostFormErrors = Partial<Record<HostFormField, string>>;

export interface HostFormValidation {
  errors: HostFormErrors;
  valid: boolean;
  /** The host to save; present only when `valid`. */
  draft?: HostDraft;
}

export const MIN_PORT = 1;
export const MAX_PORT = 65535;

export function emptyHostForm(): HostFormValues {
  return { host: "", port: String(DEFAULT_SSH_PORT), user: "", label: "" };
}

export function hostFormValues(host: {
  host: string;
  port: number;
  user: string;
  label: string;
}): HostFormValues {
  return {
    host: host.host,
    port: String(host.port),
    user: host.user,
    // §9.2's Label placeholder is "Defaults to host"; a label that is just the
    // host name is that default, so the field shows empty and stays defaulted.
    label: host.label === host.host ? "" : host.label,
  };
}

export function validateHostForm(values: HostFormValues): HostFormValidation {
  const errors: HostFormErrors = {};
  const host = values.host.trim();
  const user = values.user.trim();
  const port = values.port.trim();

  if (host.length === 0) errors.host = "Enter a hostname or IP address.";
  else if (/\s/.test(host)) errors.host = "A hostname can't contain spaces.";

  if (port.length === 0) errors.port = "Enter a port.";
  else if (!/^\d+$/.test(port)) errors.port = "A port is a whole number.";
  else {
    const value = Number(port);
    if (value < MIN_PORT || value > MAX_PORT) errors.port = "A port is between 1 and 65535.";
  }

  if (user.length === 0) errors.user = "Enter the user to log in as.";
  else if (/\s/.test(user)) errors.user = "A user name can't contain spaces.";

  const valid = Object.keys(errors).length === 0;
  return valid
    ? { errors, valid, draft: { host, port: Number(port), user, label: values.label.trim() } }
    : { errors, valid };
}
