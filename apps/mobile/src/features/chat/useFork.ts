import { useIsFocused } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";

import { refusalText } from "./refusals";

export interface ForkActions {
  /** Sends the fork of the turn the item `itemId` is in. Answers the fork's pane id; a refusal rejects. */
  fork: (itemId: string) => Promise<string>;
  /** Settles once the phone knows the fork's pane, or has waited long enough for it. */
  wait: (forkedPaneId: string) => Promise<unknown>;
  /** Shows the fork's pane as a chat. */
  open: (forkedPaneId: string) => void;
  /** Says why a fork was refused. */
  say: (message: string) => void;
}

export interface Fork {
  /** The item whose fork is on its way, or "". */
  forking: string;
  /**
   * Forks from `itemId` and opens the fork. Answers whether the fork was
   * opened: false when the host refused (which is said), when another fork is
   * still on its way, or when the reader has left the chat. It never rejects.
   */
  fork: (itemId: string) => Promise<boolean>;
}

/**
 * Fork from here (mobile-chat-mock.html, S5): the host copies the
 * conversation through the end of a turn into a new pane, and the phone opens
 * that pane as a chat, the way a side question's fork opens. One fork at a
 * time: until the host has answered, asking again does nothing. A reader who
 * left the chat meanwhile is not pulled to the fork; History lists it.
 */
export function useFork(actions: ForkActions): Fork {
  const [forking, setForking] = useState("");
  const latest = useRef(actions);
  latest.current = actions;
  const inFlight = useRef(false);
  const focused = useRef(true);
  focused.current = useIsFocused();
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const fork = useCallback(async (itemId: string) => {
    if (inFlight.current) return false;
    inFlight.current = true;
    setForking(itemId);
    try {
      const forked = await latest.current.fork(itemId);
      if (!forked) return false;
      // Opened before the topology names the fork, the chat would read as closed.
      await latest.current.wait(forked);
      if (!mounted.current || !focused.current) return false;
      latest.current.open(forked);
      return true;
    } catch (error) {
      latest.current.say(refusalText(error));
      return false;
    } finally {
      inFlight.current = false;
      if (mounted.current) setForking("");
    }
  }, []);

  return { forking, fork };
}
