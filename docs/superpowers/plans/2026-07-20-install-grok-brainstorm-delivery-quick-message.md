# Install Grok Brainstorm Delivery Quick Message Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install the approved `按 Brainstorm 端到端交付` prompt as a personal Codeg quick message in the current user's local application database.

**Architecture:** Treat the approved design spec as the single source of truth and extract its `## Final Quick Message` fenced text at execution time. Use one parameterized SQLite transaction against the existing `quick_message` table: insert when the title is absent, update when exactly one row exists with different content, no-op when it already matches, and abort when duplicate titles make intent ambiguous. Verify through a fresh read-only connection and exact string equality.

**Tech Stack:** PowerShell 7, Python 3 standard library (`pathlib`, `sqlite3`, `datetime`), SQLite WAL, existing Codeg `quick_message` schema.

## Global Constraints

- Source prompt: `docs/superpowers/specs/2026-07-20-grok-brainstorm-to-delivery-quick-message-design.md`, section `## Final Quick Message`.
- Target title: `按 Brainstorm 端到端交付`.
- Target database: `%APPDATA%\app.mycodebuddy\codeg.db`.
- Do not modify product source code or add a global/default quick-message seed.
- Do not manually duplicate the prompt outside the approved spec.
- Do not delete or merge duplicate quick-message rows; more than one matching title is a hard stop.
- Preserve an existing row's `id`, `sort_order`, and `created_at` when updating.
- Add a new row at `max(sort_order) + 1` when inserting.
- Use parameterized SQL inside `BEGIN IMMEDIATE`; roll back on every exception.
- Do not merge, push, or create a pull request.
- The plan document is the Git-tracked artifact. The installed quick message is personal application data and is intentionally not committed to the repository.

---

### Task 1: Install and verify the personal quick message

**Files:**
- Reference: `docs/superpowers/specs/2026-07-20-grok-brainstorm-to-delivery-quick-message-design.md`
- Modify data: `%APPDATA%\app.mycodebuddy\codeg.db`, table `quick_message`
- No product source files are created or modified.

**Interfaces:**
- Consumes: The first `text` code fence after `## Final Quick Message` in the approved spec.
- Produces: Exactly one `quick_message` row whose `title` is `按 Brainstorm 端到端交付` and whose `content` exactly equals the extracted fence contents after removing only trailing line breaks.
- Preserves: Existing nonmatching quick messages and, on update, the matching row's `id`, `sort_order`, and `created_at`.

- [ ] **Step 1: Run the pre-implementation working-tree gate**

Run:

```powershell
git status --short
git diff --stat
git diff --cached --stat
```

Expected: no uncommitted output after the reviewed plan has been committed. If substantial, overlapping, or unexplained changes are present, stop and ask the user whether to proceed; do not stash, commit, restore, or discard them.

- [ ] **Step 2: Confirm the live database and duplicate precondition without writing**

Run:

```powershell
@'
import json
import os
import pathlib
import sqlite3

title = "按 Brainstorm 端到端交付"
db_path = pathlib.Path(os.environ["APPDATA"]) / "app.mycodebuddy" / "codeg.db"
connection = sqlite3.connect(db_path.as_uri() + "?mode=ro", uri=True)
connection.row_factory = sqlite3.Row
connection.execute("PRAGMA query_only = ON")

try:
    journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]
    rows = connection.execute(
        "SELECT id, title, length(content) AS content_length, sort_order "
        "FROM quick_message WHERE title = ? ORDER BY id",
        (title,),
    ).fetchall()
finally:
    connection.close()

print(f"database={db_path}")
print(f"journal_mode={journal_mode}")
print("matching_rows=" + json.dumps([dict(row) for row in rows], ensure_ascii=False))
if len(rows) > 1:
    raise SystemExit("duplicate target titles require a user decision")
'@ | python -
```

Expected: `journal_mode=wal` and zero or one matching row. More than one matching row is a hard stop.

- [ ] **Step 3: Extract the approved prompt and apply one idempotent transaction**

Run:

```powershell
@'
from datetime import datetime, timezone
import os
from pathlib import Path
import sqlite3

TITLE = "按 Brainstorm 端到端交付"
SPEC_PATH = Path(
    "docs/superpowers/specs/"
    "2026-07-20-grok-brainstorm-to-delivery-quick-message-design.md"
)
DB_PATH = Path(os.environ["APPDATA"]) / "app.mycodebuddy" / "codeg.db"

spec = SPEC_PATH.read_text(encoding="utf-8")
heading = "## Final Quick Message"
if spec.count(heading) != 1:
    raise RuntimeError(f"expected exactly one {heading!r} heading")

section = spec.split(heading, 1)[1]
opener = "```text\n"
start = section.find(opener)
if start < 0:
    raise RuntimeError("approved text fence not found")
start += len(opener)
end = section.find("\n```", start)
if end < 0:
    raise RuntimeError("approved text fence is not closed")
content = section[start:end].rstrip("\r\n")

required_fragments = (
    "实施计划必须由文档审核组并行审核",
    "仅在正式执行实施计划前检查 git status 和 diff",
    "所有代码审核仅允许使用 [@Codex CLI](codeg://agent/codex)",
    "不要合并、推送或创建 PR",
)
missing = [fragment for fragment in required_fragments if fragment not in content]
if missing:
    raise RuntimeError(f"approved prompt is missing required fragments: {missing}")

connection = sqlite3.connect(DB_PATH, timeout=10.0, isolation_level=None)
connection.row_factory = sqlite3.Row
connection.execute("PRAGMA busy_timeout = 10000")

try:
    connection.execute("BEGIN IMMEDIATE")
    rows = connection.execute(
        "SELECT id, content, sort_order FROM quick_message "
        "WHERE title = ? ORDER BY id",
        (TITLE,),
    ).fetchall()
    if len(rows) > 1:
        raise RuntimeError("duplicate target titles require a user decision")

    now = datetime.now(timezone.utc).isoformat(timespec="microseconds")
    if not rows:
        next_order = connection.execute(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM quick_message"
        ).fetchone()[0]
        cursor = connection.execute(
            "INSERT INTO quick_message "
            "(title, content, sort_order, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (TITLE, content, next_order, now, now),
        )
        row_id = cursor.lastrowid
        action = "inserted"
    else:
        row_id = rows[0]["id"]
        if rows[0]["content"] == content:
            action = "unchanged"
        else:
            connection.execute(
                "UPDATE quick_message SET content = ?, updated_at = ? WHERE id = ?",
                (content, now, row_id),
            )
            action = "updated"

    connection.execute("COMMIT")
except Exception:
    if connection.in_transaction:
        connection.execute("ROLLBACK")
    raise
finally:
    connection.close()

print(f"{action}: id={row_id} title={TITLE!r} chars={len(content)}")
'@ | python -
```

Expected on the current database: output matches `^inserted: id=[1-9][0-9]* title='按 Brainstorm 端到端交付' chars=[1-9][0-9]*$`. A rerun must report `unchanged` rather than creating a duplicate.

- [ ] **Step 4: Verify exact persisted content through a fresh read-only connection**

Run:

```powershell
@'
import os
from pathlib import Path
import sqlite3

TITLE = "按 Brainstorm 端到端交付"
SPEC_PATH = Path(
    "docs/superpowers/specs/"
    "2026-07-20-grok-brainstorm-to-delivery-quick-message-design.md"
)
DB_PATH = Path(os.environ["APPDATA"]) / "app.mycodebuddy" / "codeg.db"

spec = SPEC_PATH.read_text(encoding="utf-8")
section = spec.split("## Final Quick Message", 1)[1]
opener = "```text\n"
start = section.index(opener) + len(opener)
end = section.index("\n```", start)
expected_content = section[start:end].rstrip("\r\n")

connection = sqlite3.connect(DB_PATH.as_uri() + "?mode=ro", uri=True)
connection.row_factory = sqlite3.Row
connection.execute("PRAGMA query_only = ON")
try:
    rows = connection.execute(
        "SELECT id, title, content, sort_order, created_at, updated_at "
        "FROM quick_message WHERE title = ? ORDER BY id",
        (TITLE,),
    ).fetchall()
finally:
    connection.close()

assert len(rows) == 1, f"expected one matching row, found {len(rows)}"
row = rows[0]
assert row["content"] == expected_content, "persisted content differs from approved prompt"
assert row["sort_order"] >= 0, "sort_order must be nonnegative"
assert row["created_at"], "created_at must be populated"
assert row["updated_at"], "updated_at must be populated"
print(
    f"verified: id={row['id']} chars={len(row['content'])} "
    f"sort_order={row['sort_order']}"
)
'@ | python -
```

Expected: output matches `^verified: id=[1-9][0-9]* chars=[1-9][0-9]* sort_order=[0-9]+$` with exit code 0, and the reported id and character count equal Step 3.

- [ ] **Step 5: Confirm repository isolation and report the installed record**

Run:

```powershell
git status --short
git log -3 --oneline --decorate
```

Expected: no uncommitted repository changes from the database operation. Report the quick-message title, row id, character count, exact verification result, plan/spec commits, and that no merge, push, or pull request occurred.
