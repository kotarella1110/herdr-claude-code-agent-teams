---
default: patch
---

# Have the team lead spawn helpers as teammates

The lead's appended system prompt now tells it to spawn every helper with a name when asked for a team. Without a name, Claude Code ran helpers as background subagents that never got a herdr pane.
