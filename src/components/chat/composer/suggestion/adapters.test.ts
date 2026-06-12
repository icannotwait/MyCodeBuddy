import { describe, expect, it } from "vitest"

import type { FlatFileEntry } from "@/hooks/use-file-tree"
import type {
  AcpAgentInfo,
  DbConversationSummary,
  GitLogEntry,
} from "@/lib/types"

import {
  agentToSuggestion,
  commitToSuggestion,
  fileToSuggestion,
  pathToFileUri,
  sessionToSuggestion,
} from "./adapters"

describe("pathToFileUri", () => {
  it("builds a triple-slash uri for a posix path", () => {
    expect(pathToFileUri("/repo/src/app.ts")).toBe("file:///repo/src/app.ts")
  })
  it("normalizes Windows backslashes and encodes the drive segment", () => {
    expect(pathToFileUri("C:\\repo\\app.ts")).toBe("file:///C%3A/repo/app.ts")
  })
  it("percent-encodes spaces, # and ? within segments (not the separators)", () => {
    expect(pathToFileUri("/a/b c#d?e.ts")).toBe("file:///a/b%20c%23d%3Fe.ts")
  })
})

describe("fileToSuggestion", () => {
  const entry: FlatFileEntry = {
    name: "app.ts",
    relativePath: "src/app.ts",
    kind: "file",
    lowerPath: "src/app.ts",
    lowerName: "app.ts",
  }
  it("maps to a file reference with a joined file:// uri", () => {
    const item = fileToSuggestion(entry, "/repo")
    expect(item.reference).toMatchObject({
      refType: "file",
      id: "src/app.ts",
      label: "app.ts",
      uri: "file:///repo/src/app.ts",
      meta: { fileKind: "file" },
    })
    expect(item.detail).toBe("src/app.ts")
  })
  it("does not double a separator when the root has a trailing slash", () => {
    expect(fileToSuggestion(entry, "/repo/").reference.uri).toBe(
      "file:///repo/src/app.ts"
    )
  })
})

describe("agentToSuggestion", () => {
  it("maps to an agent reference with a codeg://agent routing uri", () => {
    const agent = {
      agent_type: "claude_code",
      name: "Claude Code",
      description: "Anthropic CLI",
      available: true,
    } as AcpAgentInfo
    const item = agentToSuggestion(agent)
    expect(item.reference).toMatchObject({
      refType: "agent",
      id: "claude_code",
      label: "Claude Code",
      uri: "codeg://agent/claude_code",
      meta: { agentType: "claude_code", available: true },
    })
  })
})

describe("sessionToSuggestion", () => {
  const base = {
    id: 123,
    agent_type: "codex",
    status: "in_progress",
    git_branch: "main",
  } as DbConversationSummary
  it("encodes <agent_type>_<external_id> in the uri (id stays the numeric id)", () => {
    const item = sessionToSuggestion({
      ...base,
      title: "Login refactor",
      external_id: "abc123",
    })
    expect(item.reference).toMatchObject({
      refType: "session",
      id: "123",
      label: "Login refactor",
      uri: "codeg://session/codex_abc123",
      meta: { agentType: "codex", status: "in_progress", branch: "main" },
    })
  })
  it("falls back to the numeric id when there is no external_id", () => {
    expect(sessionToSuggestion({ ...base, title: "x" }).reference.uri).toBe(
      "codeg://session/123"
    )
  })
  it("falls back to #id when the title is empty", () => {
    expect(sessionToSuggestion({ ...base, title: null }).reference.label).toBe(
      "#123"
    )
  })
})

describe("commitToSuggestion", () => {
  it("maps to a commit reference with an encoded repo key", () => {
    const entry = {
      hash: "abc1234",
      full_hash: "abc1234def5678",
      author: "Jane",
      date: "2026-06-10",
      message: "fix login",
      files: [],
      pushed: true,
    } as GitLogEntry
    const item = commitToSuggestion(entry, "/repo with space")
    expect(item.reference).toMatchObject({
      refType: "commit",
      id: "abc1234def5678",
      label: "abc1234",
      uri: "codeg://commit/%2Frepo%20with%20space@abc1234def5678",
      meta: { shortHash: "abc1234", message: "fix login", pushed: true },
    })
  })
})

// skillToSuggestion / expertToSuggestion were retired with the `@` panel's skill
// tab — skills/commands/experts are now inserted via the `/` `$` triggers and the
// expert menu (see composer/invocation-reference.ts), not adapted for the panel.
