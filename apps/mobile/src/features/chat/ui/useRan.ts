import { useIsFocused } from "expo-router";
import { useEffect, useState } from "react";

/**
 * How long ago `since` (Unix milliseconds) was, in milliseconds, read again
 * once a second while this screen is the focused one, as the seconds turn
 * over. Undefined when there is nothing to time. A start later than this
 * phone's clock (the host's runs ahead) is no time at all, not a negative one.
 */
export function useRan(since: number | undefined): number | undefined {
  const focused = useIsFocused();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === undefined || !focused) return undefined;
    let timer: ReturnType<typeof setTimeout>;
    const tick = () => {
      const at = Date.now();
      setNow(at);
      // The next change of the seconds, not a second from whenever this ran.
      timer = setTimeout(tick, 1000 - (Math.max(0, at - since) % 1000));
    };
    tick();
    return () => clearTimeout(timer);
  }, [focused, since]);
  return since === undefined ? undefined : Math.max(0, now - since);
}
