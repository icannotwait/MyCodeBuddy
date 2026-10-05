# Roundtable rollout

The product execution gate stays disabled. Enabling it is an operator action
and is not part of this change.

- New rooms are refused while the gate is off.
- Disabling the gate drains work that already started.
- Evidence, diagnostics, and source objects are not deleted by the drain.
- This host has no Linux sandbox evidence. That is `blocked_platform`, not a pass.
