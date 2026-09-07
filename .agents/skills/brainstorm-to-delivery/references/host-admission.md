# Host Admission for Brainstorm to Delivery

This reference decides whether the Codeg protocol can be entered from the current host repository. It does not replace or relax any of the nine protocol phases, structured contracts, or validator gates in `../SKILL.md`. Inside Codeg's own repository with live orchestration tools it adds nothing; in a mirror host such as GF it is the gate.

## Required host and authorization

Read the host repository's `AGENTS.md` or equivalent instructions first. A read-only audit does not enter this workflow, initialize progress, request durable artifacts, or dispatch agents.

The workflow requires the live Codeg binding query, admission, delegation, continuation, join, and recovery capabilities described by the main skill. Discover their real schemas and required agents/profiles before document work. If unavailable, report which prerequisites are missing and leave existing workflow state intact. Ordinary DevSpace file tools are not substitutes for Codeg's durable orchestration.

The protocol requires owned commits and commit-backed reports. When the host repository defaults to local delivery without commits, explicit commit authorization is an admission prerequisite. For an ordinary authorized task without B2D admission, the host's normal local-delivery process may be used outside B2D; label it as ordinary local execution, not successful B2D delivery. An already admitted workflow retains its identity and blocks at unmet prerequisites instead of inventing commit hashes, tickets, or reviews.

## Agent roles and models

`codex`, `grok`, and profile identifiers are host routing identities, not provider model IDs. Keep the validator's role, binding, generation, and producer/reviewer contracts unchanged. A model such as `gpt-6-astra` can be selected only through a real, explicitly configured host mapping/profile that current discovery supports. Do not put a model name into a field that expects an agent route or infer that an unavailable agent exists.

Model changes inside an admitted workflow still follow the main skill's route-change and recovery protocol. Independent review means an actually separate admitted reviewer; controller self-review or a simulated child result cannot stand in for it.

## Documents and workspace ownership

Store Design, Plan, review, and progress documents where the host repository's instructions direct. Generic paths in JSON examples describe schema shape, not authority to create a new project documentation tree. Retain exact protocol markers and validator-derived bindings when using host paths.

Only the current workspace and owned changes belong to this task. Preserve user changes and unrelated workflows. Keep all artifact, approval, call-budget, reconciliation, and user-data decisions required by the main protocol. General preferences for fewer calls or faster delivery do not waive those protections.

An unavailable host is a prerequisite failure, not permission to emulate tool responses, rewrite the protocol into a single-agent run, or claim unperformed work as complete. Report the blocked work unit and any independent authorized work completed outside it separately.

## Mirrors of this skill

The Codeg repository is the single source for this directory. A mirror host copies the whole directory unchanged and keeps host-specific policy in its own repository instructions rather than in these files. Outside the Codeg repository, `scripts/validate-contract.test.mjs` skips its three Codeg-host-only fixture tests with an explicit reason; run those inside the Codeg repository.
