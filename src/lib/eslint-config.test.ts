import { ESLint } from "eslint"

describe("ESLint workspace boundaries", () => {
  it("ignores linked worktrees without ignoring the active checkout", async () => {
    const eslint = new ESLint({ cwd: process.cwd() })

    await expect(
      eslint.isPathIgnored(".worktrees/example/src/file.ts")
    ).resolves.toBe(true)
    await expect(eslint.isPathIgnored("src/lib/utils.ts")).resolves.toBe(false)
  })
})
