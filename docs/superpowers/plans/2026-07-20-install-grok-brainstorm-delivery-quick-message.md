# Install Grok Brainstorm Delivery Quick Message Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install the approved `按 Brainstorm 端到端交付` prompt as a personal Codeg quick message in the current user's local application database.

**Architecture:** Treat the approved design spec as the single source of truth and extract its `## Final Quick Message` fenced text at execution time. Each installer invocation uses one parameterized SQLite transaction against the existing `quick_message` table: insert when the title is absent, update when exactly one row exists with different content, no-op when it already matches, and abort when duplicate titles make intent ambiguous. Execute the installer twice with fresh connections to prove idempotence, and verify each commit through a fresh read-only connection and exact row equality.

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
$status = @(git status --short)
$statusText = $status -join "`n"
$statusHash = [Convert]::ToHexString(
  [Security.Cryptography.SHA256]::HashData(
    [Text.Encoding]::UTF8.GetBytes($statusText)
  )
)
"baseline_status_sha256=$statusHash"
$status
if ($status.Count -gt 0) {
  git diff --no-ext-diff
  git diff --cached --no-ext-diff
}
```

Expected on the current workspace: `baseline_status_sha256=E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855` followed by no status or diff output. Record the hash and status output. If changes exist, inspect the full unstaged and staged diffs; if they are substantial, overlapping, or unexplained, stop and ask the user whether to proceed. Do not stash, commit, restore, or discard them.

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
if not db_path.is_file():
    raise SystemExit(f"existing Codeg database not found: {db_path}")
connection = sqlite3.connect(db_path.as_uri() + "?mode=ro", uri=True)
connection.row_factory = sqlite3.Row
connection.execute("PRAGMA query_only = ON")

try:
    journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]
    columns = {
        row[1] for row in connection.execute("PRAGMA table_info(quick_message)")
    }
    required_columns = {
        "id",
        "title",
        "content",
        "sort_order",
        "created_at",
        "updated_at",
    }
    missing_columns = required_columns - columns
    if journal_mode.lower() != "wal":
        raise RuntimeError(f"expected WAL database, found {journal_mode!r}")
    if missing_columns:
        raise RuntimeError(f"quick_message is missing columns: {sorted(missing_columns)}")
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

Expected: `journal_mode=wal` and zero or one matching row, with the database file and required schema validated. More than one matching row is a hard stop.

- [ ] **Step 3: Extract the approved prompt, install it, and perform a real idempotence rerun**

Run:

```powershell
@'
from datetime import datetime, timezone
from hashlib import sha256
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

required_columns = {
    "id",
    "title",
    "content",
    "sort_order",
    "created_at",
    "updated_at",
}


def open_database(mode):
    if not DB_PATH.is_file():
        raise RuntimeError(f"existing Codeg database not found: {DB_PATH}")
    connection = sqlite3.connect(
        DB_PATH.as_uri() + f"?mode={mode}",
        uri=True,
        timeout=10.0,
        isolation_level=None,
    )
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA busy_timeout = 10000")
    return connection


def validate_database(connection):
    journal_mode = connection.execute("PRAGMA journal_mode").fetchone()[0]
    columns = {
        row[1] for row in connection.execute("PRAGMA table_info(quick_message)")
    }
    missing_columns = required_columns - columns
    if journal_mode.lower() != "wal":
        raise RuntimeError(f"expected WAL database, found {journal_mode!r}")
    if missing_columns:
        raise RuntimeError(f"quick_message is missing columns: {sorted(missing_columns)}")


def matching_rows(connection):
    return connection.execute(
        "SELECT id, title, content, sort_order, created_at, updated_at "
        "FROM quick_message WHERE title = ? ORDER BY id",
        (TITLE,),
    ).fetchall()


def install():
    connection = open_database("rw")
    try:
        validate_database(connection)
        connection.execute("BEGIN IMMEDIATE")
        rows = matching_rows(connection)
        if len(rows) > 1:
            raise RuntimeError("duplicate target titles require a user decision")

        before = dict(rows[0]) if rows else None
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

        after_rows = matching_rows(connection)
        if len(after_rows) != 1:
            raise RuntimeError(
                f"transaction produced {len(after_rows)} matching rows instead of one"
            )
        after = dict(after_rows[0])
        if after["id"] != row_id or after["title"] != TITLE:
            raise RuntimeError("transaction changed the target row identity or title")
        if after["content"] != content:
            raise RuntimeError("transaction content differs from the approved prompt")

        if action == "inserted":
            if (
                after["sort_order"] != next_order
                or after["created_at"] != now
                or after["updated_at"] != now
            ):
                raise RuntimeError("inserted row metadata differs from transaction values")
        elif action == "updated":
            for key in ("id", "sort_order", "created_at"):
                if after[key] != before[key]:
                    raise RuntimeError(f"update failed to preserve {key}")
        elif after != before:
            raise RuntimeError("unchanged install mutated the existing row")

        connection.execute("COMMIT")
    except Exception:
        if connection.in_transaction:
            connection.execute("ROLLBACK")
        raise
    finally:
        connection.close()

    verification = open_database("ro")
    verification.execute("PRAGMA query_only = ON")
    try:
        validate_database(verification)
        persisted_rows = matching_rows(verification)
    finally:
        verification.close()
    if len(persisted_rows) != 1 or dict(persisted_rows[0]) != after:
        raise RuntimeError("fresh post-commit read differs from the validated row")
    return action, after


first_action, first_row = install()
second_action, second_row = install()
if second_action != "unchanged":
    raise RuntimeError(f"idempotence rerun returned {second_action!r}")
if second_row != first_row:
    raise RuntimeError("idempotence rerun changed row identity, content, or metadata")

content_hash = sha256(content.encode("utf-8")).hexdigest()
print(
    f"first={first_action} id={first_row['id']} chars={len(content)} "
    f"content_sha256={content_hash}"
)
print(
    f"second={second_action} id={second_row['id']} chars={len(content)} "
    f"content_sha256={content_hash}"
)
'@ | python -
```

Expected on the current database: two lines. The first starts with `first=inserted`, and the second starts with `second=unchanged`. Both lines report the same positive id, positive character count, and SHA-256 digest. The second execution uses a fresh write connection and leaves the complete row unchanged.

- [ ] **Step 4: Verify exact persisted content through a fresh read-only connection**

Run:

```powershell
@'
from hashlib import sha256
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

if not DB_PATH.is_file():
    raise RuntimeError(f"existing Codeg database not found: {DB_PATH}")
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

if len(rows) != 1:
    raise RuntimeError(f"expected one matching row, found {len(rows)}")
row = rows[0]
if row["title"] != TITLE:
    raise RuntimeError("persisted title differs from the approved title")
if row["content"] != expected_content:
    raise RuntimeError("persisted content differs from the approved prompt")
content_hash = sha256(row["content"].encode("utf-8")).hexdigest()
print(
    f"verified: id={row['id']} chars={len(row['content'])} "
    f"sort_order={row['sort_order']} content_sha256={content_hash}"
)
'@ | python -
```

Expected: output reports the same positive id, character count, and content SHA-256 as both Step 3 lines, with exit code 0. `sort_order` is reported for evidence but is not constrained beyond exact preservation checks already performed inside Step 3.

- [ ] **Step 5: Confirm repository isolation and report the installed record**

Run:

```powershell
$status = @(git status --short)
$statusText = $status -join "`n"
$statusHash = [Convert]::ToHexString(
  [Security.Cryptography.SHA256]::HashData(
    [Text.Encoding]::UTF8.GetBytes($statusText)
  )
)
"final_status_sha256=$statusHash"
$status
git log -3 --oneline --decorate
```

Expected: the final status hash and output exactly match the baseline recorded in Step 1; on the current clean workspace the hash is `E3B0C44298FC1C149AFBF4C8996FB92427AE41E4649B934CA495991B7852B855` with no status lines. Report the quick-message title, row id, character count, exact verification result, plan/spec commits, and that no merge, push, or pull request occurred.
