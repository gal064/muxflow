// muxflow-managed owner=muxflow adapter=opencode
// The server half of the muxflow lifecycle plugin. It does nothing on its own.
//
// OpenCode discovers a plugin directory as one plugin and lets the terminal load
// the terminal part of any plugin the server loaded. This empty server part is
// what makes the sibling `tui.ts` (the part that actually reports lifecycle) a
// discovered plugin's terminal part. It reports nothing and reads nothing.
// Remove it from muxflow, or with `muxflow-host hook uninstall --adapter opencode`.
import { Plugin } from "@opencode/plugin";

export default Plugin.define({
  id: "muxflow.agent-status",
  setup() {},
});
