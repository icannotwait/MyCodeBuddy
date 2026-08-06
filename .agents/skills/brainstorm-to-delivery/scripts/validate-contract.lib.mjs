/**
 * Pure contract checks for brainstorm-to-delivery SKILL.md (v2 adaptive routing).
 * Structural/machine-enforceable rules live here; judgment rules stay in the Skill body.
 */

const FORBIDDEN = [
  [/workflow_manifest_v1/, "forbidden workflow_manifest_v1"],
  [/schema_version\s*[=:]\s*1/, "forbidden schema_version = 1"],
  [/pair_frozen/, "forbidden pair_frozen (use cohort_frozen)"],
  [/mode\s*=\s*legacy/i, "forbidden mode=legacy"],
]

const REQUIRED = [
  [/Codex Plan Author/, "Codex Plan Author"],
  [/writing-plans/, "writing-plans"],
  [/b2d_task_risk_v1/, "b2d_task_risk_v1"],
  [/reviewer_cohort_node_ids/, "reviewer_cohort_node_ids"],
  [/cohort_frozen/, "cohort_frozen"],
  [/holistic rewrite/i, "holistic rewrite"],
  [/user-approved requirements change/i, "user-approved requirements change"],
  [/reviewed_task_id/, "reviewed_task_id"],
  [/artifact_digest/, "artifact_digest"],
]

const RECOVERY_REQUIRED = [
  [/recovery_sources/, "recovery_sources"],
  [/actionable_task_routes/, "actionable_task_routes"],
  [/report_file/, "report_file"],
  [/get_session_info/, "get_session_info"],
  [/get_delegation_status/, "get_delegation_status"],
  [
    /inline finding summaries/i,
    "inline finding summaries compatibility warning",
  ],
]

const RECOVERY_CONTRACT_TERMS = [
  "request_recovery_authorization",
  "recovery_authorization_id",
  "recovery_confirmation_required",
  "recover_workflow",
]

function extractHeadingSection(skill, heading) {
  const escaped = heading.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  const start = skill.search(new RegExp(`^##\\s+${escaped}\\s*$`, "im"))
  if (start < 0) return null
  const from = skill.slice(start)
  const nextRel = from.slice(1).search(/^##\s/m)
  return nextRel < 0 ? from : from.slice(0, nextRel + 1)
}

function stableAgent(parsed) {
  return parsed.ok ? parsed.agent : `invalid:${parsed.reason}`
}

function canonicalNumberedRoutes(taskRouteSection) {
  const tables = parseMarkdownTablesByHeading(taskRouteSection)
  const routes = []
  for (const [heading, route] of [
    ["normal", [...tables.keys()].find((key) => /^normal route\b/i.test(key))],
    [
      "high",
      [...tables.keys()].find((key) => /^high(?:-risk)? route\b/i.test(key)),
    ],
  ]) {
    for (const row of tables.get(route) ?? []) {
      const role = classifyRole(roleCell(row.cells))
      if (!role) continue
      routes.push(
        `${heading}|${role}|${stableAgent(parseExactAgentIdentity(agentCell(row.cells)))}`
      )
    }
  }
  return routes.sort()
}

function canonicalTopRoutes(skill) {
  const section = extractHeadingSection(skill, "Codeg roles and tools")
  const tables = parseMarkdownTablesByHeading(section)
  const rows = tables.get("codeg roles and tools") ?? []
  const routes = []
  for (const row of rows) {
    if ((row.cells[0] ?? "").toLowerCase() === "route") continue
    const route = (row.cells[0] ?? "").trim().toLowerCase()
    const role = classifyRole(row.cells[1])
    if (!route || !role) continue
    routes.push(
      `${route}|${role}|${stableAgent(parseExactAgentIdentity(row.cells[2]))}`
    )
  }
  return routes.sort()
}

const EXPECTED_ROUTE_MULTISET = [
  "high|implementer|codex",
  "high|reviewer|codex",
  "high|reviewer|grok",
  "normal|implementer|grok",
  "normal|reviewer|codex",
].sort()

function sameMultiset(left, right) {
  return (
    left.length === right.length &&
    left.every((entry, index) => entry === right[index])
  )
}

function normalizedRecoveryClause(clause) {
  return clause
    .replace(/[’‘ʼ]/g, "'")
    .replace(/[`*]/g, "")
    .replace(/^\s*(?:[-+]\s+|\d+[.)]\s+)?/, "")
    .replace(/[.!?;]+\s*$/, "")
    .replace(/\s+/g, " ")
    .trim()
}

function isAuthorizationIdPrivacyClause(normalized) {
  const id = "(?:the\\s+)?recovery_authorization_id"
  const target =
    "(?:status(?:\\s+projections?)?|ledgers?|reports?|cards?|metrics|completion\\s+projections?)"
  const targets = `${target}(?:\\s*,\\s*${target})*(?:\\s*,?\\s+(?:and|or)\\s+${target})?`
  const destination = `(?:in|into|on|to)\\s+(?:the\\s+)?${targets}`
  const activeAction = "(?:persist|store|project|expose|include|write|record)"
  const passiveAction =
    "(?:persisted|stored|projected|exposed|included|written|recorded)"
  const imperativeControl =
    "(?:never|do\\s+not|must\\s+not|should\\s+not|shall\\s+not|may\\s+not|under\\s+no\\s+circumstances|must\\s+under\\s+no\\s+circumstances)"
  const activeSubject =
    "(?:the\\s+)?(?:runtime|system|workflow|implementation|validator|broker|client|codeg)"
  const subjectControl =
    "(?:(?:must|should|shall|may|does|will|can)\\s+(?:not|never)|(?:must|should|shall|may|will|can)\\s+under\\s+no\\s+circumstances|(?:doesn't|can't|won't|wouldn't|shouldn't|mustn't))"
  const passiveControl =
    `(?:` +
    `(?:must|should|shall|may|will|can)\\s+(?:not|never)\\s+be\\s+${passiveAction}|` +
    `(?:must|should|shall|may|will|can)\\s+under\\s+no\\s+circumstances\\s+be\\s+${passiveAction}|` +
    `is\\s+(?:forbidden|prohibited|disallowed)\\s+from\\s+being\\s+${passiveAction}|` +
    `is\\s+not\\s+(?:allowed|permitted)\\s+to\\s+be\\s+${passiveAction}|` +
    `isn't\\s+(?:allowed|permitted)\\s+to\\s+be\\s+${passiveAction}|` +
    `(?:must|should|shall|may)\\s+be\\s+(?:forbidden|prohibited|disallowed)\\s+from\\s+being\\s+${passiveAction}` +
    `)\\s+${destination}`

  return (
    new RegExp(
      `^${imperativeControl}\\s+${activeAction}\\s+${id}\\s+${destination}$`,
      "i"
    ).test(normalized) ||
    new RegExp(
      `^${activeSubject}\\s+${subjectControl}\\s+${activeAction}\\s+${id}\\s+${destination}$`,
      "i"
    ).test(normalized) ||
    new RegExp(
      `^${id}\\s+${passiveControl}(?:\\s+and\\s+${passiveControl})*$`,
      "i"
    ).test(normalized)
  )
}

function isSafeNeutralRecoveryClause(normalized, token) {
  if (token === "recovery_authorization_id") {
    return isAuthorizationIdPrivacyClause(normalized)
  }
  if (token === "recover_workflow") {
    return (
      /^(?:an?\s+)?enabled\s+catalog\s+missing\s+recover_workflow\s+hard[- ]blocks?$/i.test(
        normalized
      ) ||
      /^recover_workflow\s+(?:never|(?:(?:must|should|does|do|will|would|can|could|may)\s+not))\s+(?:generates?|creates?|opens?|mints?)\s+(?:a\s+)?challenge$/i.test(
        normalized
      ) ||
      /^Require the full v2 tool set \(get_workflow_capabilities, get_workflow_state, publish_workflow_manifest, settle_workflow_gate, recover_workflow\)$/i.test(
        normalized
      ) ||
      /^(?:["”]\s*\|\s*)?Workflow recovery requires recover_workflow$/i.test(
        normalized
      )
    )
  }
  return false
}

function hasExplicitNegativeRecoverySemantics(normalized) {
  return (
    /\bnot\b/i.test(normalized) ||
    /\b(?:isn't|aren't|wasn't|weren't|can't|don't|doesn't|won't|wouldn't|shouldn't|mustn't)\b/i.test(
      normalized
    ) ||
    /\b(?:never|cannot|forbid(?:s|den|ding)?|prohibit(?:s|ed|ing)?|disallow(?:s|ed|ing)?|avoid(?:s|ed|ing)?)\b/i.test(
      normalized
    ) ||
    /\bby\s+no\s+means\b/i.test(normalized) ||
    /\bunder\s+no\s+circumstances\b/i.test(normalized) ||
    /\bwithout\b/i.test(normalized)
  )
}

function isExplicitPositiveRecoveryClause(normalized, token) {
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  const delegationTable =
    "^\\|\\s*Typed delegation recovery challenge\\s*\\|\\s*" +
    "Projected call\\s*(?:->|→)\\s*recovery_confirmation_required\\s*" +
    "(?:->|→)\\s*request_recovery_authorization\\s*(?:->|→)\\s*" +
    "exact rejected call replay with the ID$"
  const workflowTable =
    "^\\|\\s*Workflow recovery\\s*\\|\\s*get_workflow_state\\s*" +
    "(?:->|→)\\s*request_recovery_authorization\\s*(?:->|→)\\s*" +
    "receipt-required recover_workflow$"

  if (token === "recovery_confirmation_required") {
    return (
      new RegExp(
        `^(?:receive(?:s|d)?|emit(?:s|ted)?|surface(?:s|d)?)\\s+` +
          `(?:a\\s+)?typed\\s+${escaped}` +
          `(?:\\s+from\\s+(?:the\\s+)?projected call)?$`,
        "i"
      ).test(normalized) || new RegExp(delegationTable, "i").test(normalized)
    )
  }
  if (token === "request_recovery_authorization") {
    return (
      new RegExp(
        `^(?:then\\s+)?(?:call|invoke)\\s+${escaped}` +
          `(?:\\s+for\\s+(?:the\\s+)?rejected action)?$`,
        "i"
      ).test(normalized) ||
      new RegExp(delegationTable, "i").test(normalized) ||
      new RegExp(workflowTable, "i").test(normalized)
    )
  }
  if (token === "recovery_authorization_id") {
    const id = `(?:the\\s+)?${escaped}`
    const replayKind =
      "(?:recovery|continue|replacement|recover_workflow|continue\\s+or\\s+replacement)"
    const replayLed = new RegExp(
      `^(?:then\\s+)?replay\\s+the\\s+exact\\s+rejected\\s+` +
        `${replayKind}\\s+call\\s+(?:` +
        `(?:with|using)\\s+${id}(?:\\s+and\\s+the\\s+same\\s+key,\\s*profile,\\s*and\\s*action)?|` +
        `and\\s+(?:supply|pass|use)\\s+${id}(?:\\s+as\\s+(?:an?\\s+)?input)?` +
        `)$`,
      "i"
    )
    const inputLed = new RegExp(
      `^(?:supply|pass|use)\\s+${id}(?:\\s+as\\s+(?:an?\\s+)?input)?\\s+` +
        `(?:on|to|for)\\s+the\\s+exact\\s+rejected\\s+` +
        `(?:recovery|continue|replacement|recover_workflow)\\s+replay\\s+call$`,
      "i"
    )
    return replayLed.test(normalized) || inputLed.test(normalized)
  }
  if (token === "recover_workflow") {
    return (
      new RegExp(
        `^(?:(?:then|after authorization,)\\s+)?` +
          `(?:call|invoke)\\s+receipt-required\\s+${escaped}$`,
        "i"
      ).test(normalized) ||
      new RegExp(workflowTable, "i").test(normalized) ||
      new RegExp(
        `^(?:supply|pass|use)\\s+(?:the\\s+)?recovery_authorization_id` +
          `(?:\\s+as\\s+(?:an?\\s+)?input)?\\s+(?:on|to|for)\\s+` +
          `the\\s+exact\\s+rejected\\s+${escaped}\\s+replay\\s+call$`,
        "i"
      ).test(normalized) ||
      new RegExp(
        `^(?:then\\s+)?replay\\s+the\\s+exact\\s+rejected\\s+${escaped}` +
          `\\s+call\\s+(?:(?:with|using)\\s+(?:the\\s+)?` +
          `recovery_authorization_id(?:\\s+and\\s+the\\s+same\\s+key,` +
          `\\s*profile,\\s*and\\s*action)?|and\\s+(?:supply|pass|use)` +
          `\\s+(?:the\\s+)?recovery_authorization_id` +
          `(?:\\s+as\\s+(?:an?\\s+)?input)?)$`,
        "i"
      ).test(normalized)
    )
  }
  return false
}

function recoveryClauses(skill) {
  return skill.split(/\r?\n\s*\r?\n/).flatMap((paragraph) => {
    const lines = paragraph.split(/\r?\n/).filter((line) => line.trim())
    const bounded = lines.every((line) => /^\s*\|/.test(line))
      ? lines
      : [lines.join(" ")]
    return bounded.flatMap((clause) => clause.split(/(?<=[.!?;])/))
  })
}

function recoveryTokenMentions(skill, token) {
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  const exactToken = new RegExp(
    `(^|[^A-Za-z0-9_])${escaped}(?![A-Za-z0-9_])`,
    "g"
  )
  const mentions = []
  const clauses = recoveryClauses(skill)
  for (const clause of clauses) {
    exactToken.lastIndex = 0
    if (!exactToken.test(clause)) continue
    const normalized = normalizedRecoveryClause(clause)
    const negative = hasExplicitNegativeRecoverySemantics(normalized)
    const safeNeutral = isSafeNeutralRecoveryClause(normalized, token)
    const polarity = negative
      ? safeNeutral
        ? "neutral"
        : "negated"
      : safeNeutral
        ? "neutral"
        : isExplicitPositiveRecoveryClause(normalized, token)
          ? "affirmative"
          : "invalid"
    mentions.push({ polarity })
  }
  return mentions
}

function affirmativeTokenMention(skill, token) {
  return recoveryTokenMentions(skill, token).some(
    (mention) => mention.polarity === "affirmative"
  )
}

function hasInvalidTokenMention(skill, token) {
  return recoveryTokenMentions(skill, token).some(
    (mention) =>
      mention.polarity === "negated" || mention.polarity === "invalid"
  )
}

function hasUnsafeCancellationMapping(skill) {
  const causes = [
    "parent_canceled",
    "parent_turn_failed",
    "join_abandoned",
    "user_cancelled",
    "tool_stalled_timeout",
  ]
  return skill.split(/[.,;\r\n]+/).some((clause) => {
    const lower = clause.toLowerCase()
    const matches = [...lower.matchAll(new RegExp(causes.join("|"), "g"))]
    return matches.some((match, index) => {
      const cause = match[0]
      const causeIndex = match.index
      const previousEnd =
        index === 0
          ? 0
          : matches[index - 1].index + matches[index - 1][0].length
      const nextStart = matches[index + 1]?.index ?? lower.length
      const before = lower.slice(
        Math.max(previousEnd, causeIndex - 48),
        causeIndex
      )
      const after = lower.slice(causeIndex + cause.length, nextStart)
      if (!/(?:unresumable|replacement(?:_reason)?|replace)/.test(after)) {
        return false
      }
      if (
        /(?:never|must not|does not|do not|cannot|can't)\s+(?:map|replace|use)\b[^,.;]*$/.test(
          before
        ) ||
        /(?:never\s+maps?|must not\s+(?:map|replace|use)|does not\s+(?:map|replace|use)|do not\s+(?:map|replace|use)|cannot\s+(?:map|replace|use)|can't\s+(?:map|replace|use)|is not\s+(?:a\s+)?replacement source)\b/.test(
          after
        )
      ) {
        return false
      }
      return /(?:maps?|use[sd]?|becomes?|is|to|replacement_reason\s*=|replace)/.test(
        after
      )
    })
  })
}

function hasNegatedDesignTrigger(skill, trigger) {
  const escaped = trigger.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  return new RegExp(
    `${escaped}\\b[^.;\\r\\n]{0,80}\\b(?:` +
      `(?:does|do|should|must|may|can|could|will|would)\\s+(?:not|never)\\s+trigger|` +
      `never\\s+triggers?|cannot\\s+trigger|can't\\s+trigger` +
      `)\\s+(?:an\\s+)?external Design review`,
    "i"
  ).test(skill)
}

function normalizedCompletionClauses(skill) {
  return skill
    .split(/\r?\n\s*\r?\n/)
    .flatMap((paragraph) =>
      paragraph
        .split(/\r?\n/)
        .map((line) => line.trim())
        .filter(Boolean)
        .join(" ")
        .split(/(?<=[.!?;])/)
    )
    .map((clause) => clause.replace(/[`*_]/g, "").replace(/\s+/g, " ").trim())
    .filter(Boolean)
}

function isFrozenV1Clause(clause) {
  if (/\b(?:protocol[- ]?v?2|v2)\b/i.test(clause)) return false
  const subject =
    "(?:frozen\\s+(?:protocol[- ]?v?1|v1)(?:\\s+historical)?\\s+(?:workflow|branch)|" +
    "(?:protocol[- ]?v?1|v1)\\s+historical\\s+(?:workflow|branch))"
  const prefix = clause.match(
    new RegExp(
      `^(?:(?:for|within|under)\\s+(?:the\\s+)?(?:explicit\\s+)?${subject},\\s*|` +
        `(?:the\\s+)?(?:explicit\\s+)?${subject}\\b)`,
      "i"
    )
  )
  if (!prefix) return false

  const remainder = clause.slice(prefix[0].length)
  return !/[,:]|\b(?:but|however|yet|whereas|while|although|though|and|then)\b/i.test(
    remainder
  )
}

function negatesActionOnObject(clause, actions, object) {
  const target = `\\b(?:${actions})\\b[^.!?;]{0,160}(?:${object})`
  return new RegExp(
    `(?:never|do\\s+not|does\\s+not|must\\s+not|should\\s+not|` +
      `may\\s+not|shall\\s+not|cannot|can't)\\s+` +
      `(?:\\w+\\s+){0,4}${target}|` +
      `\\bno\\s+(?:\\w+\\s+){1,3}${target}|` +
      `\\bunder\\s+no\\s+circumstances\\s+` +
      `(?:\\w+\\s+){0,3}${target}|` +
      `(?:is|are)\\s+forbidden\\s+to\\s+(?:\\w+\\s+){0,4}${target}`,
    "i"
  ).test(clause)
}

function hasUnsafeCardTemplate(skill) {
  const actions =
    "emits?|requests?|requires?|asks?|includes?|suppl(?:y|ies)|produces?|provides?|returns?|uses?"
  const object =
    "codeg-card-summary-v1|card(?:\\s+(?:template|block|summary|evidence|settlement))?"
  return normalizedCompletionClauses(skill).some((clause) => {
    if (isFrozenV1Clause(clause)) return false
    if (
      !new RegExp(`\\b(?:${actions})\\b[^.!?;]{0,160}(?:${object})`, "i").test(
        clause
      )
    ) {
      return false
    }
    return !negatesActionOnObject(clause, actions, object)
  })
}

function hasUnsafeCardHarvestSettlement(skill) {
  const card =
    "codeg-card-summary-v1|(?:missing\\s+chat\\s+)?card(?:\\s+(?:template|block|summary|evidence|settlement))?"
  const harvest = "harvest(?:s|ed|ing)?"
  const activeHarvest = "harvest(?:s|ing)?"
  const actor = "platform|parent|workflow|worker|child|reviewer"
  return normalizedCompletionClauses(skill).some((clause) => {
    if (isFrozenV1Clause(clause)) return false

    const directHarvest = new RegExp(
      `\\b(?:${activeHarvest})\\b[^.!?;]{0,160}(?:${card}|it\\b)|` +
        `\\b(?:${actor})\\b(?!-)[^.!?;]{0,80}\\bharvested\\b` +
        `[^.!?;]{0,160}(?:${card}|it\\b)`,
      "i"
    ).test(clause)
    if (
      directHarvest &&
      !negatesActionOnObject(clause, harvest, `${card}|it\\b`)
    ) {
      return true
    }

    const cardSettlement = new RegExp(
      `(?:${card})\\s+(?:` +
        `(?:(?:may|can|must|should|will)\\s+)?settles?\\b|` +
        `(?:(?:may|can|must|should|will)\\s+)?authorizes?\\b` +
        `[^.!?;]{0,80}settlement\\b|` +
        `(?:is|remains|becomes|provides?|constitutes?|serves\\s+as)\\s+` +
        `(?:a\\s+)?(?:sufficient\\s+|valid\\s+|authoritative\\s+)?` +
        `settlement(?:\\s+evidence)?\\b` +
        `)`,
      "i"
    ).exec(clause)
    if (cardSettlement) {
      const prefix = clause.slice(0, cardSettlement.index)
      if (!/\bno(?:\s+[\w-]+){0,4}\s*$/i.test(prefix)) return true
    }

    const settlementActions = "uses?|accepts?|treats?"
    const cardAsSettlement =
      `${card}[^.!?;]{0,80}\\b(?:as|for)\\s+` +
      `(?:(?:valid|sufficient|authoritative)\\s+)?` +
      `settlement(?:\\s+evidence)?\\b`
    const usesCardAsSettlement = new RegExp(
      `\\b(?:${settlementActions})\\b[^.!?;]{0,160}(?:${cardAsSettlement})`,
      "i"
    ).test(clause)
    if (
      usesCardAsSettlement &&
      !negatesActionOnObject(clause, settlementActions, cardAsSettlement)
    ) {
      return true
    }

    const settleAction = "settle(?:s|d|ing)?"
    const settlesFromCard = new RegExp(
      `\\b(?:${settleAction})\\b[^.!?;]{0,160}(?:${card})`,
      "i"
    ).test(clause)
    return settlesFromCard && !negatesActionOnObject(clause, settleAction, card)
  })
}

function hasUnsafeDigestRequest(skill) {
  const actions =
    "asks?|requests?|requires?|tells?|provides?|returns?|suppl(?:y|ies)"
  const actor = "child|worker|model|author|reviewer"
  const digest = "(?:artifact\\s+)?digest"
  const object =
    `(?:${actor})[^.!?;]{0,100}${digest}|` +
    `${digest}[^.!?;]{0,100}(?:from|by)\\s+(?:the\\s+)?(?:${actor})`
  return normalizedCompletionClauses(skill).some((clause) => {
    if (isFrozenV1Clause(clause)) return false
    const asksForDigest = new RegExp(
      `(?:${actions})\\b[^.!?;]{0,180}(?:${object})|` +
        `(?:${actor})\\b[^.!?;]{0,100}(?:${actions})\\b[^.!?;]{0,100}${digest}`,
      "i"
    ).test(clause)
    if (!asksForDigest) return false
    return !negatesActionOnObject(clause, actions, digest)
  })
}

function hasUnsafeFormatContinuation(skill) {
  const clauses = normalizedCompletionClauses(skill)
  const actions =
    "continue(?:_delegation)?(?:s|d|ing)?|resumes?|reopens?|retr(?:y|ies|ied|ying)"
  const object =
    "(?:missing|malformed|invalid|unavailable|unclear)[^.!?;]{0,80}(?:completion|format|summary|report)|" +
    "(?:completion|format|summary|report)[^.!?;]{0,80}(?:missing|malformed|invalid|unavailable|unclear)"
  return clauses.some((clause) => {
    if (isFrozenV1Clause(clause)) return false
    const unsafe = new RegExp(
      `(?:${actions})\\b[^.!?;]{0,180}(?:${object})|` +
        `(?:${object})[^.!?;]{0,180}(?:${actions})\\b`,
      "i"
    ).test(clause)
    if (!unsafe) return false
    return !negatesActionOnObject(clause, actions, object)
  })
}

function hasUnsafeReemitOrTerminalRecovery(skill) {
  const clauses = normalizedCompletionClauses(skill)
  return clauses.some((clause) => {
    if (/CARD\s+RE-EMIT\s+ONLY/i.test(clause)) return true
    if (isFrozenV1Clause(clause)) return false
    const actions = "continues?|replaces?|reopens?|re-emits?|reemits?"
    const object =
      "(?:semantically\\s+)?terminal\\s+child|completed\\s+child|(?:completion\\s+)?card"
    if (
      !new RegExp(`(?:${actions})\\b[^.!?;]{0,160}(?:${object})`, "i").test(
        clause
      )
    ) {
      return false
    }
    return !negatesActionOnObject(clause, actions, object)
  })
}

function hasUnsafeGateCountReduction(skill) {
  return normalizedCompletionClauses(skill).some((clause) => {
    if (isFrozenV1Clause(clause)) return false
    return /(?:Design|Plan|Task|Final)[^.]{0,80}(?:gate|review)[^.]{0,120}(?:advances?|passes?|settles?|approves?)[^.]{0,120}(?:Critical|Important|findings?|counts?)/i.test(
      clause
    )
  })
}

function completionContractState(skill) {
  const plain = skill.replace(/[`*]/g, " ").replace(/\s+/g, " ")
  return {
    workerConclusion:
      /protocol-v2 workflow[^.]{0,120}workers? (?:call|use) complete_work[^.]{0,160}(?:explicit terminal|terminal or report conclusion)[^.]{0,80}otherwise/i.test(
        plain
      ),
    platformAuthority:
      /Parent advances only from platform completion\.state and workflow admission state/i.test(
        plain
      ),
    durableAttention:
      /completion\.state is needs_decision or artifact_recovery[^.]{0,160}durable typed attention[^.]{0,80}wait/i.test(
        plain
      ),
    rootReentry:
      /After resolution or a user continuation turn[^.]{0,180}reload workflow state at the root[^.]{0,160}re-enter gate settlement or admission/i.test(
        plain
      ) &&
      /Never continue, replace, or reopen the semantically terminal child/i.test(
        plain
      ),
    cleanBaseline:
      /Before every (?:protocol-v2 )?Implementer or Final Fixer admission[^.]{0,260}resolve HEAD[^.]{0,160}git status --porcelain[^.]{0,100}exactly empty[^.]{0,160}producer_baseline_head/i.test(
        plain
      ),
    rejectsUnrelatedDirt:
      /there is no unrelated-dirt allowance/i.test(plain) &&
      !/(?:may|can|should|does)\s+(?:allow|accept|ignore)[^.]{0,80}unrelated[- ]dirt/i.test(
        plain
      ),
    producerCommit:
      /passing (?:Implementer or Final Fixer|producer) completion requires a clean workflow-owned (?:producer )?commit[^.]{0,180}(?:different from|differs from) (?:that|the) baseline/i.test(
        plain
      ) &&
      /durable (?:Task policy )?allow_noop_verification[^.]{0,100}(?:authorize|authorizes)/i.test(
        plain
      ) &&
      /(?:Task and Final code Reviewers|Code Reviewers)[^.]{0,180}(?:validate|re-resolve)[^.]{0,160}clean HEAD[^.]{0,180}(?:admission and completion|admission[^.]{0,80}completion)/i.test(
        plain
      ),
    finalAggregation:
      /Finish all Final history aggregation[^.]{0,120}before Final Reviewer admission/i.test(
        plain
      ) &&
      !/Admit (?:the )?Final Reviewer before Final history aggregation/i.test(
        plain
      ),
    finalFreeze:
      /passing Final freezes the (?:reviewed )?delivery HEAD[^.]{0,180}(?:through delivery and reporting|through delivery|frozen commit)/i.test(
        plain
      ) &&
      /Post-settlement (?:HEAD )?drift[^.]{0,100}final_artifact_drift[^.]{0,120}reopen(?:s)? Final/i.test(
        plain
      ),
    protocolBoundary:
      /(?:explicit )?v1 historical branch|workflow whose frozen completion protocol remains v1/i.test(
        plain
      ) &&
      (/(?:v2 successor)[^.]{0,160}no v1 evidence or settlement (?:may )?cross(?:es)? into it/i.test(
        plain
      ) ||
        /no v1 evidence or settlement (?:may )?cross(?:es)? into (?:a |the )?v2 successor/i.test(
          plain
        )),
    platformGateOutcomes:
      /Design, Plan, Task, and Final gates advance from platform outcomes and validated scope/i.test(
        plain
      ) && !hasUnsafeGateCountReduction(skill),
    finalFindingsPackage:
      /For a non-pass Final[^.]{0,180}consume only the platform Final-findings package and context[^.]{0,120}Final Fixer/i.test(
        plain
      ),
  }
}

/**
 * Isolate the numbered Task route section (`## 4. Task route` or `## Task route`).
 * Returns null when the section is missing.
 */
export function extractTaskRouteSection(skill) {
  const start = skill.search(/^##\s*(?:\d+\.\s*)?Task route\b/im)
  if (start < 0) return null
  const from = skill.slice(start)
  // Next ## heading after this one (not the current line).
  const nextRel = from.slice(1).search(/^##\s/m)
  if (nextRel < 0) return from.trimEnd() + (from.endsWith("\n") ? "\n" : "")
  return from.slice(0, nextRel + 1)
}

/**
 * Parse Markdown tables under a subsection heading inside `section`.
 * Returns Map of heading-lower -> array of {cells: string[]}
 */
export function parseMarkdownTablesByHeading(section) {
  const result = new Map()
  if (!section) return result

  const lines = section.split(/\r?\n/)
  let currentHeading = null
  let tableRows = null

  const flush = () => {
    if (currentHeading && tableRows && tableRows.length) {
      result.set(currentHeading, tableRows)
    }
    tableRows = null
  }

  for (const raw of lines) {
    const line = raw.trimEnd()
    const heading = line.match(/^#{2,4}\s+(.+?)\s*$/)
    if (heading) {
      flush()
      currentHeading = heading[1].trim().toLowerCase()
      continue
    }
    if (!/^\|/.test(line.trim())) {
      if (tableRows) flush()
      continue
    }
    const cells = line
      .trim()
      .replace(/^\|/, "")
      .replace(/\|$/, "")
      .split("|")
      .map((c) => c.trim())
    // separator row
    if (cells.every((c) => /^:?-{3,}:?$/.test(c))) {
      if (!tableRows) tableRows = []
      continue
    }
    if (!tableRows) tableRows = []
    tableRows.push({ cells })
  }
  flush()
  return result
}

function roleCell(cells) {
  return (cells[0] || "").toLowerCase()
}

function agentCell(cells) {
  return cells[1] || ""
}

/**
 * Explicitly harmless parenthetical annotations only (fail closed otherwise).
 * Reject agent identities, alternatives, fallbacks, or unknown notes.
 */
export function isHarmlessAgentParenthetical(inner) {
  const t = String(inner ?? "")
    .replace(/[*_`~]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .toLowerCase()
  if (!t) return false

  // Never allow a second identity or alternative/fallback semantics inside notes.
  if (/\bgrok\b|\bcodex\b/.test(t)) return false
  if (
    /\b(or|and|\/|fallback|alternative|instead|either|optional|prefer|else|otherwise)\b/.test(
      t
    )
  ) {
    return false
  }

  // Allowlist only the annotations used by the Skill route tables.
  if (/^independent(?:\s+child)?$/.test(t)) return true
  // e.g. ≠ implementer, ≠ Author  (unicode or ascii not-equal)
  if (/^[≠!=]+\s*implementer(?:\s*[,;]\s*[≠!=]+\s*author)?$/.test(t)) {
    return true
  }
  if (/^[≠!=]+\s*author(?:\s*[,;]\s*[≠!=]+\s*implementer)?$/.test(t)) {
    return true
  }
  if (
    /^[≠!=]+\s*implementer\s*[,;]\s*[≠!=]+\s*author$/.test(t) ||
    /^[≠!=]+\s*author\s*[,;]\s*[≠!=]+\s*implementer$/.test(t)
  ) {
    return true
  }
  return false
}

/**
 * Strip only allowed annotations/punctuation, then require exactly one
 * canonical agent identity (`grok` | `codex`). No substring membership.
 * @returns {{ ok: true, agent: "grok"|"codex" } | { ok: false, reason: string }}
 */
export function parseExactAgentIdentity(raw) {
  let s = String(raw ?? "")

  // Validate each parenthetical before stripping; only harmless annotations drop.
  const parenRe = /\(([^)]*)\)/g
  let m
  while ((m = parenRe.exec(s)) !== null) {
    if (!isHarmlessAgentParenthetical(m[1])) {
      return {
        ok: false,
        reason: `disallowed parenthetical annotation: (${m[1].trim()})`,
      }
    }
  }
  s = s.replace(/\([^)]*\)/g, " ")

  // Drop Markdown emphasis
  s = s.replace(/[*_`~]+/g, " ")
  // Normalize separators that signal alternatives / lists
  s = s.replace(/[/|,;&]+/g, " ")
  s = s.replace(/\b(?:or|and|\/)\b/gi, " ")
  s = s.replace(/\s+/g, " ").trim().toLowerCase()

  if (!s) {
    return { ok: false, reason: "empty agent cell" }
  }

  // Tokenize on whitespace; only exact tokens "grok" and "codex" count.
  const tokens = s.split(" ").filter(Boolean)
  const agents = []
  const unknown = []
  for (const t of tokens) {
    // Allow benign role words that sometimes appear after stripping notes
    if (
      /^(?:agent|type|independent|child|implementer|fixer|reviewer|author|profile|none)$/.test(
        t
      )
    ) {
      continue
    }
    if (t === "grok" || t === "codex") {
      agents.push(t)
      continue
    }
    unknown.push(t)
  }

  if (unknown.length) {
    return {
      ok: false,
      reason: `non-canonical agent token(s): ${unknown.join(" ")}`,
    }
  }
  const unique = [...new Set(agents)]
  if (unique.length === 0) {
    return { ok: false, reason: "no canonical agent identity" }
  }
  if (unique.length > 1) {
    return {
      ok: false,
      reason: `mixed agent identities: ${unique.join(" / ")}`,
    }
  }
  return { ok: true, agent: unique[0] }
}

function classifyRole(roleRaw) {
  const role = String(roleRaw || "")
    .toLowerCase()
    .replace(/[*_`~]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
  if (!role || role === "role") return null
  if (/implementer/.test(role)) return "implementer"
  if (/reviewer/.test(role)) return "reviewer"
  return "other"
}

/**
 * Assert normal/high route tables map roles to exact agents.
 * @returns {string[]} failure messages
 */
export function validateRouteTables(taskRouteSection) {
  const failures = []
  if (!taskRouteSection) {
    failures.push(
      "missing Task route section (expected `## Task route` or `## N. Task route`)"
    )
    return failures
  }

  const tables = parseMarkdownTablesByHeading(taskRouteSection)
  const normalKey = [...tables.keys()].find((k) => /^normal route\b/i.test(k))
  const highKey = [...tables.keys()].find((k) =>
    /^high(?:-risk)? route\b/i.test(k)
  )

  if (!normalKey) {
    failures.push("Task route missing `### Normal route` table")
  } else {
    const rows = tables
      .get(normalKey)
      .filter((r) => classifyRole(roleCell(r.cells)))
    const impls = []
    const revs = []
    const extras = []
    for (const r of rows) {
      const kind = classifyRole(roleCell(r.cells))
      const parsed = parseExactAgentIdentity(agentCell(r.cells))
      if (kind === "implementer") {
        if (!parsed.ok) {
          failures.push(
            `normal route implementer cell is not exact: ${parsed.reason}`
          )
        } else {
          impls.push(parsed.agent)
        }
      } else if (kind === "reviewer") {
        if (!parsed.ok) {
          failures.push(
            `normal route reviewer cell is not exact: ${parsed.reason}`
          )
        } else {
          revs.push(parsed.agent)
        }
      } else {
        extras.push(roleCell(r.cells))
      }
    }

    if (impls.length !== 1 || revs.length !== 1 || extras.length > 0) {
      failures.push(
        `normal route must have exactly one implementer row and one reviewer row (got implementer=${impls.length}, reviewer=${revs.length}, extra=${extras.length})`
      )
    }
    if (impls.length === 1 && impls[0] !== "grok") {
      failures.push(
        "normal route table must map Implementer/fixer exactly to Grok"
      )
    }
    if (revs.length === 1 && revs[0] !== "codex") {
      failures.push(
        "normal route table must map Independent reviewer exactly to Codex"
      )
    }
    // Keep legacy-style message for pure wrong-agent single-token failures
    if (impls.length === 1 && impls[0] === "codex") {
      failures.push("normal route implementer must be Grok, not Codex")
    }
  }

  if (!highKey) {
    failures.push("Task route missing `### High route` table")
  } else {
    const rows = tables
      .get(highKey)
      .filter((r) => classifyRole(roleCell(r.cells)))
    const impls = []
    const revs = []
    const extras = []
    for (const r of rows) {
      const kind = classifyRole(roleCell(r.cells))
      const parsed = parseExactAgentIdentity(agentCell(r.cells))
      if (kind === "implementer") {
        if (!parsed.ok) {
          failures.push(
            `high route implementer cell is not exact: ${parsed.reason}`
          )
        } else {
          impls.push(parsed.agent)
        }
      } else if (kind === "reviewer") {
        if (!parsed.ok) {
          failures.push(
            `high route reviewer cell is not exact: ${parsed.reason}`
          )
        } else {
          revs.push(parsed.agent)
        }
      } else {
        extras.push(roleCell(r.cells))
      }
    }

    if (impls.length !== 1) {
      failures.push(
        `high route must have exactly one implementer row (got ${impls.length})`
      )
    } else if (impls[0] !== "codex") {
      failures.push(
        "high route table must map Implementer/fixer exactly to Codex"
      )
    }

    if (revs.length !== 2) {
      failures.push(
        `high route table must list exactly two distinct Independent reviewer rows (got ${revs.length})`
      )
    } else {
      const set = new Set(revs)
      if (!(set.has("codex") && set.has("grok"))) {
        failures.push(
          "high route table must assign exactly independent Codex AND Grok reviewers"
        )
      }
      if (revs[0] === revs[1]) {
        failures.push(
          "high route must use two distinct reviewer agents (not the same agent twice)"
        )
      }
    }

    if (extras.length > 0) {
      failures.push(
        `high route has unexpected extra role row(s): ${extras.join(", ")}`
      )
    }
  }

  return failures
}

const ACTION_FORMS = new Map([
  ["write", "write"],
  ["writes", "write"],
  ["writing", "write"],
  ["wrote", "write"],
  ["author", "author"],
  ["authors", "author"],
  ["authoring", "author"],
  ["authored", "author"],
  ["draft", "draft"],
  ["drafts", "draft"],
  ["drafting", "draft"],
  ["drafted", "draft"],
  ["compose", "compose"],
  ["composes", "compose"],
  ["composing", "compose"],
  ["composed", "compose"],
  ["generate", "generate"],
  ["generates", "generate"],
  ["generating", "generate"],
  ["generated", "generate"],
  ["rewrite", "rewrite"],
  ["rewrites", "rewrite"],
  ["rewriting", "rewrite"],
  ["rewrote", "rewrite"],
  ["edit", "edit"],
  ["edits", "edit"],
  ["editing", "edit"],
  ["edited", "edit"],
  ["implement", "implement"],
  ["implements", "implement"],
  ["implementing", "implement"],
  ["implemented", "implement"],
  ["invoke", "invoke"],
  ["invokes", "invoke"],
  ["invoking", "invoke"],
  ["invoked", "invoke"],
])

const CONTENT_ACTIONS = new Set([
  "write",
  "author",
  "draft",
  "compose",
  "generate",
  "rewrite",
  "edit",
])
const CONTRAST_BOUNDARIES = new Set(["but", "however", "yet", "then"])
const DELEGATION_CONTROL_FORMS = new Map([
  ["ask", "ask"],
  ["asks", "ask"],
  ["dispatch", "dispatch"],
  ["dispatches", "dispatch"],
  ["instruct", "instruct"],
  ["instructs", "instruct"],
  ["require", "require"],
  ["requires", "require"],
  ["tell", "tell"],
  ["tells", "tell"],
])
const DELEGATED_CHILD_SUBJECTS = new Set(["author", "implementer", "reviewer"])
const NON_PARENT_SUBJECTS = new Set([
  "author",
  "child",
  "codex",
  "fixer",
  "grok",
  "implementer",
  "reviewer",
])
const PROGRESSIVE_AUXILIARIES = new Set([
  "am",
  "are",
  "be",
  "been",
  "being",
  "is",
  "was",
  "were",
])
const AFFIRMATIVE_MODALS = new Set([
  "am",
  "are",
  "can",
  "could",
  "did",
  "do",
  "does",
  "is",
  "may",
  "might",
  "must",
  "shall",
  "should",
  "was",
  "were",
  "will",
  "would",
])
const OBJECT_BOUNDARIES = new Set([
  ".",
  ",",
  ";",
  ":",
  "!",
  "?",
  ")",
  "/",
  "and",
  "or",
  ...CONTRAST_BOUNDARIES,
  "after",
  "as",
  "before",
  "because",
  "by",
  "during",
  "for",
  "if",
  "once",
  "to",
  "under",
  "unless",
  "until",
  "when",
  "where",
  "while",
  "with",
  "without",
])
const OBJECT_TRAILING_MODIFIERS = new Set([
  "directly",
  "immediately",
  "itself",
  "personally",
  "quickly",
  "urgently",
])

function normalizeOwnershipText(text) {
  return String(text ?? "")
    .replace(/[*_`~]+/g, "")
    .replace(/[\t ]+/g, " ")
}

function tokenizeOwnershipText(text) {
  return (
    text.match(/[a-z]+(?:['’][a-z]+)?(?:-[a-z]+)*|[-.,;:!?()/]/gi) ?? []
  ).map((raw) => ({ raw, value: raw.toLowerCase() }))
}

function nextObjectToken(tokens, actionIndex) {
  let index = actionIndex + 1
  if (tokens[index]?.value === "the") index += 1
  return index
}

function objectEndsAt(tokens, index) {
  let cursor = index + 1
  while (OBJECT_TRAILING_MODIFIERS.has(tokens[cursor]?.value)) cursor += 1
  if (tokens[cursor]?.value === "(") {
    const modifierStart = ++cursor
    while (OBJECT_TRAILING_MODIFIERS.has(tokens[cursor]?.value)) cursor += 1
    if (cursor === modifierStart || tokens[cursor]?.value !== ")") return false
    cursor += 1
  }
  const next = tokens[cursor]?.value
  return next === undefined || OBJECT_BOUNDARIES.has(next) || next === "now"
}

function progressiveHasAuxiliary(tokens, actionIndex) {
  let cursor = actionIndex - 1
  while (
    tokens[cursor]?.value === "not" ||
    tokens[cursor]?.value === "now" ||
    OBJECT_TRAILING_MODIFIERS.has(tokens[cursor]?.value)
  ) {
    cursor -= 1
  }
  return PROGRESSIVE_AUXILIARIES.has(tokens[cursor]?.value)
}

/** Return a label only for the exact protected object owned by the action. */
function classifyProtectedAction(tokens, actionIndex) {
  const actionToken = tokens[actionIndex]?.value
  const action = ACTION_FORMS.get(actionToken)
  if (!action) return null
  if (
    actionToken === "writing" &&
    !progressiveHasAuxiliary(tokens, actionIndex)
  ) {
    return null
  }

  const objectIndex = nextObjectToken(tokens, actionIndex)
  const object = tokens[objectIndex]?.value

  if (action === "invoke") {
    return object === "writing-plans" && objectEndsAt(tokens, objectIndex)
      ? "parent invokes writing-plans (Author must)"
      : null
  }

  if (action === "implement") {
    if (object !== "task" && object !== "tasks") return null
    const codeIndex = objectIndex + 1
    if (tokens[codeIndex]?.value === "code") {
      return objectEndsAt(tokens, codeIndex) ? "Parent implements Task" : null
    }
    return objectEndsAt(tokens, objectIndex) ? "Parent implements Task" : null
  }

  if (!CONTENT_ACTIONS.has(action)) return null
  if (object === "plan") {
    const containerIndex = objectIndex + 1
    if (["file", "document"].includes(tokens[containerIndex]?.value)) {
      return objectEndsAt(tokens, containerIndex) ? "Parent writes Plan" : null
    }
    return objectEndsAt(tokens, objectIndex) ? "Parent writes Plan" : null
  }

  if (object === "task" || object === "tasks") {
    const codeIndex = objectIndex + 1
    if (
      tokens[codeIndex]?.value === "code" &&
      objectEndsAt(tokens, codeIndex)
    ) {
      return "Parent writes Task code"
    }
  }
  return null
}

function tokenStartsProhibition(token) {
  return (
    token === "not" ||
    token === "never" ||
    token === "cannot" ||
    token === "without" ||
    token === "forbidden" ||
    token === "prohibited" ||
    /n't$/.test(token)
  )
}

function startsDelegatedChildComplement(tokens, controlIndex) {
  const controlVerb = DELEGATION_CONTROL_FORMS.get(tokens[controlIndex]?.value)
  if (!controlVerb) return false

  let childIndex = controlIndex + 1
  if (tokens[childIndex]?.value === "the") childIndex += 1
  return (
    DELEGATED_CHILD_SUBJECTS.has(tokens[childIndex]?.value) &&
    tokens[childIndex + 1]?.value === "to"
  )
}

function scanParentSpan(tokens, found) {
  let pendingProhibition = false
  let sharedProhibition = false
  let pendingCoordinate = false
  let clauseStart = false
  let delegatedChildComplement = false

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index].value

    if (delegatedChildComplement) {
      if (!CONTRAST_BOUNDARIES.has(token)) continue
      delegatedChildComplement = false
      pendingProhibition = false
      sharedProhibition = false
      pendingCoordinate = false
      clauseStart = true
      continue
    }
    if (startsDelegatedChildComplement(tokens, index)) {
      delegatedChildComplement = true
      pendingProhibition = false
      sharedProhibition = false
      pendingCoordinate = false
      clauseStart = false
      continue
    }
    if (clauseStart && NON_PARENT_SUBJECTS.has(token)) break
    if (token === ".") break
    if (token === ",") {
      if (!sharedProhibition) pendingProhibition = false
      pendingCoordinate = false
      clauseStart = true
      continue
    }
    if (
      CONTRAST_BOUNDARIES.has(token) ||
      token === ";" ||
      token === ":" ||
      token === "-" ||
      token === "/"
    ) {
      pendingProhibition = false
      sharedProhibition = false
      pendingCoordinate = false
      clauseStart = true
      continue
    }
    if (AFFIRMATIVE_MODALS.has(token)) {
      pendingProhibition = false
      sharedProhibition = false
      pendingCoordinate = false
      continue
    }
    if (tokenStartsProhibition(token)) {
      pendingProhibition = true
      sharedProhibition = false
      pendingCoordinate = false
      continue
    }
    if (token === "and" || token === "or") {
      if (!sharedProhibition && !pendingCoordinate) {
        pendingProhibition = false
      }
      pendingCoordinate = false
      clauseStart = true
      continue
    }

    if (
      clauseStart &&
      !ACTION_FORMS.has(token) &&
      !PROGRESSIVE_AUXILIARIES.has(token) &&
      !OBJECT_TRAILING_MODIFIERS.has(token) &&
      !["a", "an", "not", "now", "the", "to"].includes(token)
    ) {
      pendingProhibition = false
      sharedProhibition = false
      pendingCoordinate = false
      clauseStart = false
    }

    const label = classifyProtectedAction(tokens, index)
    if (label) {
      const prohibited = pendingProhibition || sharedProhibition
      if (!prohibited) {
        found.push(`parent authorship permission present: ${label}`)
      }
      if (pendingProhibition) sharedProhibition = true
      pendingProhibition = false
      pendingCoordinate = false
      clauseStart = false
      continue
    }

    if (ACTION_FORMS.has(token)) {
      clauseStart = false
      pendingCoordinate =
        pendingProhibition && ["and", "or"].includes(tokens[index + 1]?.value)
      if (!pendingCoordinate) {
        pendingProhibition = false
        sharedProhibition = false
      }
    }
  }
}

function findEnglishParentActions(line, found) {
  const tokens = tokenizeOwnershipText(line)
  const parentIndexes = tokens
    .map((token, index) => (token.value === "parent" ? index : -1))
    .filter((index) => index >= 0)

  for (let index = 0; index < parentIndexes.length; index += 1) {
    const start = parentIndexes[index] + 1
    const end = parentIndexes[index + 1] ?? tokens.length
    scanParentSpan(tokens.slice(start, end), found)
  }
}

/**
 * Detect direct Plan/Task authoring or implementation permission statements.
 * Independent of modal verbs. Urgency clauses do not exempt. Negation is
 * evaluated against each matched action instead of a punctuation-based clause.
 */
export function findParentPermissionViolations(skill) {
  const found = []
  const lines = normalizeOwnershipText(skill).split(/\r?\n/)

  for (const line of lines) {
    if (!line.trim()) continue
    findEnglishParentActions(line, found)

    if (
      /使用\s*writing-plans\s*编写任何实施计划/.test(line) &&
      !/(?:不得|禁止|不要|不得亲自)\s*$/.test(
        line.slice(0, line.search(/使用\s*writing-plans/))
      )
    ) {
      found.push(
        "parent authorship permission present: parent instructed to invoke writing-plans itself"
      )
    }
    // This bounded parser is defense in depth for known ownership grammar;
    // it is not proof of arbitrary natural-language ownership semantics.
    const chineseAction = line.match(
      /父会话[^。；;]*?(起草|拟写|编写|撰写|创作|生成|改写|重写|编辑|修改)[^。；;]*?(Plan|Task\s*code|代码)/i
    )
    const chinesePrefix = chineseAction
      ? line.slice(
          line.indexOf("父会话"),
          chineseAction.index + chineseAction[0].indexOf(chineseAction[1])
        )
      : ""
    if (
      chineseAction &&
      !/(?:不得|禁止|不要|不应|不可|不能)[^。；;]*$/i.test(chinesePrefix)
    ) {
      found.push(
        "parent authorship permission present: parent Chinese permission to author Plan/Task"
      )
    }
    if (
      /父会话.*writing-plans/.test(line) &&
      !/父会话.*(?:不得|禁止|不要)/.test(line)
    ) {
      found.push(
        "parent authorship permission present: parent Chinese writing-plans ownership"
      )
    }
  }

  return [...new Set(found)]
}

/**
 * Reject any parent Plan/Task authorship permission; require explicit bans.
 * @returns {{ failures: string[], notes: string[] }}
 */
export function validateParentOwnership(skill) {
  const failures = []
  const notes = []

  for (const msg of findParentPermissionViolations(skill)) {
    failures.push(msg)
  }

  // Author-owns alone must never mask a permission violation already collected.
  const authorOwns =
    /Author owns the Plan/i.test(skill) ||
    /Codex Plan Author owns every Plan/i.test(skill)

  if (
    authorOwns &&
    failures.some((f) => /parent authorship permission present/i.test(f))
  ) {
    failures.push(
      "contradictory parent Plan/Task authorship: Author-owns cannot mask parent write/implement permission"
    )
  }

  const parentNoTaskCode =
    /parent must not implement Task code/i.test(skill) ||
    /父会话不得亲自\s*实现|不得由父会话直接实现/i.test(skill) ||
    /parent.*must not.*(?:implement|write).*(?:Task code|Plan)/i.test(skill)

  if (!parentNoTaskCode) {
    failures.push("missing ban: parent must not implement Task code")
  } else {
    notes.push("parent must not implement Task code")
  }

  const parentNoPlanWrite =
    /parent must not write or rewrite the Plan/i.test(skill) ||
    /Parent must not write or rewrite the Plan/i.test(skill) ||
    /父会话不得(?:直接)?(?:改写|编写|撰写|重写)(?:实施)?计划/i.test(skill)

  if (!parentNoPlanWrite) {
    failures.push(
      "missing ban: parent must not write/rewrite the Plan (Author owns it)"
    )
  } else {
    notes.push("Plan authorship owned by Author, not parent")
  }

  if (!authorOwns) {
    failures.push("missing statement that Codex Plan Author owns the Plan")
  }

  return { failures, notes }
}

/**
 * Validate full skill markdown. Returns { failures, notes }.
 */
export function validateSkillMarkdown(skill) {
  const failures = []
  const notes = []
  const lines = skill.split(/\r?\n/)

  const fail = (ruleId, msg) => failures.push(`[${ruleId}] ${msg}`)
  const pass = (msg) => notes.push(`OK: ${msg}`)

  // --- Forbidden: reject every literal occurrence (brief raw patterns) ---
  for (const [re, label] of FORBIDDEN) {
    if (re.test(skill)) {
      fail("B2D-001", label)
    } else {
      pass(label.replace(/^forbidden /, "absent "))
    }
  }

  for (const [re, label] of REQUIRED) {
    if (!re.test(skill)) {
      fail("B2D-002", `missing required term: ${label}`)
    } else {
      pass(`has ${label}`)
    }
  }

  for (const [re, label] of RECOVERY_REQUIRED) {
    if (!re.test(skill)) {
      fail("B2D-003", `missing required recovery term: ${label}`)
    } else {
      pass(`has ${label}`)
    }
  }

  const fm = skill.match(/^---\r?\n([\s\S]*?)\r?\n---/)
  if (!fm) {
    fail("B2D-004", "missing YAML frontmatter")
  } else {
    const descMatch = fm[1].match(/^description:\s*(.+)$/m)
    if (!descMatch) {
      fail("B2D-004", "frontmatter missing description")
    } else {
      const desc = descMatch[1].trim()
      if (
        /\b(Codex Plan Author|b2d_task_risk|normal route|high route|stagnation)\b/i.test(
          desc
        )
      ) {
        fail(
          "B2D-004",
          "frontmatter description must be trigger-only (leaks workflow terms)"
        )
      } else {
        pass("frontmatter description present")
      }
      if (!/^Use when\b/i.test(desc)) {
        fail("B2D-004", 'frontmatter description should start with "Use when"')
      }
    }
  }

  if (lines.length >= 500) {
    fail("B2D-005", `SKILL.md has ${lines.length} lines (must be < 500)`)
  } else {
    pass(`line count ${lines.length} < 500`)
  }

  const unsafeCardHarvestSettlement = hasUnsafeCardHarvestSettlement(skill)
  if (hasUnsafeCardTemplate(skill) || unsafeCardHarvestSettlement) {
    fail(
      "B2D-COMP-001",
      "v2 guidance emits, requests, harvests, or settles from a completion Card"
    )
  } else {
    pass("v2 completion Card templates and settlement authority are absent")
  }
  if (hasUnsafeDigestRequest(skill)) {
    fail("B2D-COMP-002", "requests a model-authored artifact digest")
  } else {
    pass("model-authored digest requests are absent")
  }
  if (hasUnsafeFormatContinuation(skill)) {
    fail("B2D-COMP-003", "continues a child to repair completion format")
  } else {
    pass("format-only completion continuation is absent")
  }
  if (hasUnsafeReemitOrTerminalRecovery(skill)) {
    fail(
      "B2D-COMP-004",
      "uses completion re-emission or reopens a semantically terminal child"
    )
  } else {
    pass("completion re-emission and terminal-child recovery are absent")
  }

  const completion = completionContractState(skill)
  if (!completion.cleanBaseline) {
    fail(
      "B2D-COMP-005",
      "missing clean resolvable Implementer/Final-Fixer admission baseline"
    )
  } else {
    pass("clean producer admission baseline")
  }
  if (!completion.rejectsUnrelatedDirt) {
    fail("B2D-COMP-006", "unrelated worktree dirt is not rejected")
  } else {
    pass("no unrelated-dirt allowance")
  }
  if (!completion.producerCommit) {
    fail(
      "B2D-COMP-007",
      "missing workflow-owned producer commit or Reviewer HEAD validation"
    )
  } else {
    pass("workflow-owned producer commit and Reviewer HEAD validation")
  }
  if (!completion.finalAggregation) {
    fail(
      "B2D-COMP-008",
      "Final aggregation must finish before Final Reviewer admission"
    )
  } else {
    pass("Final aggregation precedes Final Reviewer admission")
  }
  if (!completion.finalFreeze) {
    fail("B2D-COMP-009", "missing passing Final delivery commit freeze")
  } else {
    pass("passing Final freezes the delivery commit")
  }
  if (!completion.platformAuthority) {
    fail(
      "B2D-COMP-010",
      "Parent must advance only from platform completion and admission state"
    )
  } else {
    pass("platform completion state is authoritative")
  }
  if (!completion.durableAttention) {
    fail("B2D-COMP-011", "missing durable completion attention wait")
  } else {
    pass("durable completion attention waits")
  }
  if (!completion.rootReentry) {
    fail(
      "B2D-COMP-012",
      "missing root reload/re-entry after completion attention"
    )
  } else {
    pass("completion resolution re-enters from root state")
  }
  if (!completion.protocolBoundary) {
    fail("B2D-COMP-013", "missing explicit frozen v1/v2 evidence boundary")
  } else {
    pass("frozen v1 history is isolated from v2 successors")
  }
  if (!completion.workerConclusion) {
    fail(
      "B2D-COMP-014",
      "missing protocol-v2 worker completion conclusion contract"
    )
  } else {
    pass("protocol-v2 worker terminal conclusion contract")
  }
  if (!completion.platformGateOutcomes) {
    fail(
      "B2D-COMP-015",
      "Design/Plan/Task/Final gates must reduce platform outcomes and scope"
    )
  } else {
    pass("all v2 gates reduce platform outcomes and scope")
  }
  if (!completion.finalFindingsPackage) {
    fail(
      "B2D-COMP-016",
      "non-pass Final must use only the platform Final-findings package"
    )
  } else {
    pass("non-pass Final uses the platform findings package")
  }

  const ownership = validateParentOwnership(skill)
  for (const f of ownership.failures) fail("B2D-006", f)
  for (const n of ownership.notes) pass(n)

  const taskRouteSection = extractTaskRouteSection(skill)
  const routeFailures = validateRouteTables(taskRouteSection)
  if (routeFailures.length === 0) {
    pass("normal route table Grok implementer + Codex reviewer")
    pass("high route table Codex implementer + two distinct reviewers")
  } else {
    for (const f of routeFailures) fail("B2D-007", f)
  }

  const topRoutes = canonicalTopRoutes(skill)
  const numberedRoutes = canonicalNumberedRoutes(taskRouteSection)
  if (!sameMultiset(topRoutes, EXPECTED_ROUTE_MULTISET)) {
    fail(
      "B2D-013",
      `top Codeg roles and tools table is not exact: ${topRoutes.join(", ")}`
    )
    fail("B2D-007", "an authoritative route table is not exact")
  } else {
    pass("top Codeg roles and tools table is exact")
  }
  if (!sameMultiset(topRoutes, numberedRoutes)) {
    fail("B2D-014", "top and numbered route surfaces differ")
  } else {
    pass("route surfaces are identical")
  }

  // High must not allow single-reviewer pass (permissive language in whole skill)
  const highAllowsOneReviewer =
    /high[\s\S]{0,200}?\b(may|can|should|enough to)\b[\s\S]{0,80}?\b(pass|ship|approve)\b[\s\S]{0,80}?\b(one|single)\b[\s\S]{0,40}?reviewer/i.test(
      skill
    ) ||
    /\b(one|single)\b\s+reviewer\s+(is\s+)?enough\b/i.test(skill) ||
    /pass high with (only\s+)?one reviewer/i.test(skill)

  if (highAllowsOneReviewer) {
    fail("B2D-008", "high route must not allow passing with a single reviewer")
  }

  const coverage =
    /reviewed_task_id/.test(skill) &&
    /artifact_digest/.test(skill) &&
    /latest/i.test(skill)
  if (!coverage) {
    fail(
      "B2D-009",
      "missing exact latest producer coverage (reviewed_task_id + artifact_digest)"
    )
  } else {
    pass("latest producer coverage terms present")
  }

  const planContracts = [
    [
      /platform-selected Plan nodes|platform-selected reviewer set/i,
      "platform-selected Plan review nodes",
    ],
    [
      /platform[^.]{0,120}(?:full-group|complete cohort|complete Plan review group)[^.]{0,120}lineage/i,
      "platform-owned full-group lineage",
    ],
    [
      /platform-selected[\s\S]{0,80}holistic\s+rewrite/i,
      "holistic rewrite route",
    ],
    [
      /user-approved requirements change[\s\S]{0,100}(?:receipt|lineage)/i,
      "durable requirements-change lineage",
    ],
    [
      /pre-admission|before.*cohort.*admitted/i,
      "pre-admission risk correction path",
    ],
    [
      /post-admission|after any cohort.*admitted|cohort_frozen/i,
      "post-admission freeze",
    ],
    [/consolidated fix|one consolidated/i, "consolidated fix request"],
    [
      /both\s+reviewers[\s\*]*re-review|Both reviewers perform|both\s+prior\s+reviews|both reviewers re-review the latest|全部审核者[\s\S]{0,20}复审/i,
      "both-reviewers-after-fix",
    ],
  ]

  for (const [re, label] of planContracts) {
    if (!re.test(skill)) {
      fail("B2D-010", `missing contract: ${label}`)
    } else {
      pass(label)
    }
  }

  if (!/subagent-driven-development/.test(skill)) {
    fail("B2D-011", "must invoke subagent-driven-development by name")
  } else {
    pass("invokes subagent-driven-development")
  }

  const quickReference =
    extractHeadingSection(skill, "Quick reference under pressure") ?? ""
  const phaseLines = [
    "Design Gate approved -> dispatch Plan Author automatically.",
    "Plan Gate approved -> run Workspace gate, then dispatch the first eligible Task automatically.",
    "Task Gate passed -> dispatch the next eligible Task or Final review automatically.",
    "Final review approved -> deliver and report the frozen commit automatically.",
  ]
  for (const line of phaseLines) {
    if (!quickReference.includes(line)) {
      fail("B2D-012", `missing automatic phase transition: ${line}`)
    }
  }
  for (const condition of [
    "hard block",
    "user_decision_required",
    "requirements, scope, architecture, or user data handling",
  ]) {
    if (!quickReference.includes(condition)) {
      fail("B2D-012", `missing exact pause condition: ${condition}`)
    }
  }
  if ((skill.match(/user_decision_required/g) ?? []).length < 2) {
    fail(
      "B2D-012",
      "user_decision_required must appear in both recovery and pause contracts"
    )
  }
  if (
    /pause[^.\n]{0,80}(?:user approval|approval)[^.\n]{0,80}(?:phase|next)/i.test(
      quickReference
    )
  ) {
    fail(
      "B2D-012",
      "adds a user-approval pause outside the exact hard conditions"
    )
  }

  for (const token of RECOVERY_CONTRACT_TERMS) {
    if (
      !affirmativeTokenMention(skill, token) ||
      hasInvalidTokenMention(skill, token)
    ) {
      fail("B2D-R001", `missing affirmative recovery contract term: ${token}`)
    }
  }

  const delegationSequenceValid =
    /projected call[\s\S]{0,160}recovery_confirmation_required[\s\S]{0,160}request_recovery_authorization[\s\S]{0,220}replay the exact rejected (?:continue or\s*)?replacement call|projected call[\s\S]{0,160}recovery_confirmation_required[\s\S]{0,160}request_recovery_authorization[\s\S]{0,220}replay the exact rejected continue or\s*replacement call/i.test(
      skill
    ) && /same key, profile, and\s*action/i.test(skill)
  const unsafeDelegationSequence =
    /request_recovery_authorization before recovery_confirmation_required/i.test(
      skill
    ) ||
    /similar replacement call instead of replaying the exact rejected call/i.test(
      skill
    ) ||
    /change the key and profile before replaying the action/i.test(skill)
  if (!delegationSequenceValid || unsafeDelegationSequence) {
    fail(
      "B2D-R002",
      "delegation challenge, authorization, and exact replay order is invalid"
    )
  }

  if (hasUnsafeCancellationMapping(skill)) {
    fail(
      "B2D-R003",
      "cancellation or stall evidence maps affirmatively to replacement/unresumable"
    )
  }

  if (
    !/tool_stalled_timeout[\s\S]{0,100}confirmed same-key continue/i.test(
      skill
    ) ||
    /tool_stalled_timeout continues (?:without confirmation|automatically)/i.test(
      skill
    ) ||
    /tool_stalled_timeout uses replacement before continue/i.test(skill)
  ) {
    fail(
      "B2D-R004",
      "tool_stalled_timeout must be confirmed, resume-first, and same-key"
    )
  }

  const workflowSequenceValid =
    /Workflow recovery follows this exact ordered recipe:[\s\S]{0,100}get_workflow_state[\s\S]{0,120}request_recovery_authorization[\s\S]{0,120}receipt-required recover_workflow/i.test(
      skill
    ) &&
    /enabled catalog missing recover_workflow hard-blocks/i.test(skill) &&
    /recover_workflow never\s+generates a challenge/i.test(skill)
  if (
    !workflowSequenceValid ||
    /recover_workflow before request_recovery_authorization/i.test(skill) ||
    /skip get_workflow_state/i.test(skill) ||
    /missing recover_workflow may proceed/i.test(skill)
  ) {
    fail(
      "B2D-R005",
      "workflow status, authorization, recovery, or catalog contract is invalid"
    )
  }

  if (
    !/user_decision_required requires exact reset_plan_lineage[\s\S]{0,180}displayed reason hash/i.test(
      skill
    ) ||
    !/receipt is the durable[\s\S]{0,100}reason[\s\S]{0,100}new authorized stagnation baseline/i.test(
      skill
    ) ||
    /user_decision_required may reset[\s\S]{0,180}without reset_plan_lineage/i.test(
      skill
    )
  ) {
    fail(
      "B2D-R006",
      "user_decision_required lineage reset lacks exact receipt/reason/baseline"
    )
  }

  if (
    !/First admission freezes the key, role, agent, profile[\s\S]{0,120}inherited continue[\s\S]{0,80}replacement counters/i.test(
      skill
    ) ||
    !/Recovery never changes key\/profile or resets inherited\s*consumption/i.test(
      skill
    ) ||
    /Recovery may change the admitted key or profile/i.test(skill) ||
    /Recovery resets inherited continue and replacement consumption/i.test(
      skill
    )
  ) {
    fail(
      "B2D-R007",
      "admitted identity/profile or inherited counters are mutable"
    )
  }

  if (
    !completion.platformAuthority ||
    !completion.durableAttention ||
    !completion.rootReentry ||
    unsafeCardHarvestSettlement ||
    /Reject platform completion\.state and parse the child conclusion directly/i.test(
      skill
    ) ||
    /Prose approval settles the completion decision/i.test(skill) ||
    /After needs_decision, continue the terminal child instead of root re-entry/i.test(
      skill
    )
  ) {
    fail(
      "B2D-R008",
      "platform completion attention or root re-entry contract is invalid"
    )
  }

  const designTriggers = [
    "migration",
    "security/authorization",
    "concurrency",
    "persistence/state-machine",
    "externally visible compatibility",
    "ambiguity",
  ]
  if (
    !/Normal Task review independently recomputes b2d_task_risk_v1/i.test(
      skill
    ) ||
    designTriggers.some((trigger) => !skill.toLowerCase().includes(trigger)) ||
    /Normal Task review copies b2d_task_risk_v1/i.test(skill) ||
    designTriggers.some((trigger) => hasNegatedDesignTrigger(skill, trigger))
  ) {
    fail(
      "B2D-R009",
      "Task risk recomputation or deterministic Design trigger is invalid"
    )
  }

  if (
    !/Exhausted continue uses same-key budget_exhausted_continue replacement[\s\S]{0,120}replacement budget remains/i.test(
      skill
    ) ||
    !/after replacement consumption, block/i.test(skill) ||
    /continue exhaustion, mint a new key and profile/i.test(skill) ||
    /continue exhaustion, replace with replacement_reason=unresumable/i.test(
      skill
    ) ||
    /After replacement consumption, replace again/i.test(skill)
  ) {
    fail("B2D-R010", "continue/replacement exhaustion contract is invalid")
  }

  if (
    !/Before every delegation or continue, write ledger intent[\s\S]{0,120}intended key,[\s\S]{0,80}role, agent, profile, and action/i.test(
      skill
    ) ||
    !/Fill latest_task_id after admission/i.test(skill) ||
    !/reconcile from platform state after recovery/i.test(skill) ||
    /Write ledger intent after the delegation mutation/i.test(skill) ||
    /Ledger intent may omit intended action and identity/i.test(skill) ||
    /Skip platform-state reconciliation after recovery/i.test(skill)
  ) {
    fail(
      "B2D-R011",
      "ledger intent or post-recovery reconciliation contract is invalid"
    )
  }

  return { failures, notes }
}

export { FORBIDDEN, REQUIRED }
