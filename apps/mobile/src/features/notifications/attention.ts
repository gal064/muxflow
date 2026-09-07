/**
 * The agent screen a person is currently looking at. This is deliberately
 * separate from `focusedPaneId`: terminal focus also owns host visibility and
 * resource-seeding semantics, while this value only affects notifications.
 */
export interface NotificationAttention {
  focusAgent(agentId: string): () => void;
  setAppActive(active: boolean): void;
  viewedAgentId(): string | undefined;
}

export function createNotificationAttention(initiallyActive = true): NotificationAttention {
  let appActive = initiallyActive;
  let focused: { agentId: string; owner: object } | undefined;

  return {
    focusAgent(agentId) {
      const owner = {};
      focused = { agentId, owner };
      return () => {
        // A late blur from an old route must not clear the route now on top.
        if (focused?.owner === owner) focused = undefined;
      };
    },
    setAppActive(active) {
      appActive = active;
    },
    viewedAgentId() {
      return appActive ? focused?.agentId : undefined;
    },
  };
}

export const notificationAttention = createNotificationAttention();
