# Upstream Synchronization

MyCodeBuddy tracks the upstream [Codeg](https://github.com/xintaofei/codeg)
repository while preserving fork-specific release policy and functionality.

## Remote Setup

Configure the original Codeg repository as `upstream` and MyCodeBuddy as
`origin`:

```bash
git remote rename origin upstream
git remote add origin https://github.com/icannotwait/MyCodeBuddy.git
git fetch --all --prune
git config rerere.enabled true
```

## Sync Flow

Create a merge branch from the current MyCodeBuddy `main` and merge upstream
history without rebasing:

```bash
git fetch upstream
git switch main
git pull --ff-only origin main
git switch -c sync/codeg-0.20.2
git merge --no-ff upstream/main
```

Resolve conflicts on the sync branch, then open a pull request into
MyCodeBuddy `main`. Do not rebase published MyCodeBuddy history. Run the full
repository verification suite before merging the pull request.

For an upstream Codeg `0.20.2` sync, reset the fork version in `package.json`,
`src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and
`src-tauri/tauri.conf.json` to `0.20.2-mycodebuddy.1`. Run
`pnpm test:release` to verify that the versions and runtime URLs remain
consistent.

## Conflict Priorities

Resolve conflicts in this order:

1. Preserve MyCodeBuddy branding, version suffixes, repository metadata,
   updater endpoints, download links, installers, and user-facing links.
2. Preserve deletion of OpenClaw code unless a MyCodeBuddy change explicitly
   restores a reviewed replacement.
3. Preserve the MyCodeBuddy release workflow and its Windows-only release,
   signing, compliance, and repository policy.
4. Preserve functional fork changes, then adapt incoming upstream changes
   around them instead of silently discarding either behavior.

After conflict resolution, review all branding/updater files, deleted OpenClaw
paths, the release workflow, and functional fork diffs explicitly in the pull
request. Complete the frontend and Rust checks required by `AGENTS.md`, in
addition to `pnpm test:release`, before merge.
