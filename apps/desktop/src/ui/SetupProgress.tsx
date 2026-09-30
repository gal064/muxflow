/** Immediate feedback for a setup operation the user has just started. */
export function SetupProgress({ detail }: { detail: string }) {
  return <p className="setup-progress" role="status">
    <span aria-hidden="true" className="spinner" />{detail}
  </p>;
}
