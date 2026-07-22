# Codeg Delegation Tool Discovery Guard Design

Date: 2026-07-22

Status: Design approved in conversation; written-spec review pending

## Summary

Strengthen the repository-local `brainstorm-to-delivery` skill so an agent does
not confuse Codex native collaboration with Codeg cross-agent delegation. Grok
and Codex role routing must use Codeg `delegate_to_agent`; inability to select
Grok through `collaboration.spawn_agent` is not evidence that Grok is
unavailable.

## Incident Evidence

Conversation `1078` loaded the skill and had an active Codeg root companion,
but used only the native `spawn_agent` family. It then reported a false hard
block because that family cannot select Grok. The deferred Codeg MCP tool was
not discovered even though its input accepts `agent_type: "grok"`.

## Root Cause

The skill specifies role agent types but does not specify the delegation tool
contract or the evidence required before declaring an agent unavailable. This
leaves the model free to treat the first visible sub-agent API as the complete
capability surface.

## Selected Design

Add a compact, mandatory Codeg routing gate immediately after the role table in
`.agents/skills/brainstorm-to-delivery/SKILL.md`:

1. Distinguish `collaboration.spawn_agent` from Codeg `delegate_to_agent`.
2. Route Grok implementers and fixers through `delegate_to_agent` with
   `agent_type: "grok"`; route Codex reviewers through the same Codeg tool with
   `agent_type: "codex"`.
3. When Codeg MCP tools are deferred rather than directly listed, inspect
   `ALL_TOOLS` and invoke the matching tool through the available tool
   namespace.
4. Report a delegation blocker only after the Codeg tool is confirmed absent
   or an actual Codeg delegation call returns an unavailable/error result.
5. Add the observed false inference to the skill's common-excuses table.

Keep the rule in this repository skill. Do not modify global
`subagent-driven-development`, because the Grok/Codex role policy belongs to
this delivery workflow and should not affect unrelated projects.

## Testing

Use the observed conversation as the RED baseline: without the new guard, an
agent sees only `spawn_agent`, concludes Grok cannot be selected, and stops.

Forward-test the revised skill with a fresh agent under combined pressure:

- only native collaboration tools appear prominently;
- Codeg MCP delegation is deferred behind tool discovery;
- the task requires Grok and says to stop if Grok is genuinely unavailable.

Passing behavior discovers Codeg `delegate_to_agent`, selects
`agent_type: "grok"`, and reserves the blocker path for confirmed absence or an
actual failed delegation call.

## Acceptance Criteria

1. The skill names the correct Codeg delegation API and both required agent
   types.
2. It explicitly rejects `spawn_agent` capability as proof of Grok
   availability.
3. It requires deferred-tool discovery before reporting a blocker.
4. A fresh-context forward test chooses Codeg delegation rather than falsely
   stopping.
5. No global skill or unrelated project behavior changes.
