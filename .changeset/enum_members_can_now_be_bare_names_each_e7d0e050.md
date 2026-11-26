---
nessemble: minor
---

`.enum` members can now be bare names, each taking the next value (`OP_DRAW` on its own line, in the first column), and `.enum START, STEP, NAME` names the enum and defines `NAME_COUNT`. **Behaviour change:** a bare name inside an `.enum` was silently the constant `1`; it is now a member.
