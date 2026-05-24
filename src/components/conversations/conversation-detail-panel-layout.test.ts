import { readFileSync } from "node:fs"
import { resolve } from "node:path"

const source = readFileSync(
  resolve(
    process.cwd(),
    "src/components/conversations/conversation-detail-panel.tsx"
  ),
  "utf8"
)
const welcomeHeroSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/welcome-hero.tsx"),
  "utf8"
)
const chatInputSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/chat-input.tsx"),
  "utf8"
)
const messageInputSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/message-input.tsx"),
  "utf8"
)
const conversationShellSource = readFileSync(
  resolve(process.cwd(), "src/components/chat/conversation-shell.tsx"),
  "utf8"
)

describe("ConversationDetailPanel new conversation layout", () => {
  it("keeps the new-conversation input in the welcome panel with the original scroll layout", () => {
    expect(source).toContain(
      "hideInput={isWelcomeMode || Boolean(acpLoadError)}"
    )

    const welcomeBranchStart = source.indexOf("{isWelcomeMode ? (")
    const nextBranchStart = source.indexOf(
      ") : showDraftHeader ?",
      welcomeBranchStart
    )

    expect(welcomeBranchStart).toBeGreaterThan(-1)
    expect(nextBranchStart).toBeGreaterThan(welcomeBranchStart)

    const welcomeBranch = source.slice(welcomeBranchStart, nextBranchStart)
    expect(welcomeBranch).toContain("<ChatInput")
    expect(welcomeBranch).toContain("overflow-x-hidden overflow-y-auto")
    expect(welcomeBranch).not.toContain("WelcomeBackdrop")
  })

  it("does not render a decorative welcome backdrop", () => {
    expect(welcomeHeroSource).not.toContain("export function WelcomeBackdrop")
    expect(welcomeHeroSource).not.toContain("bg-gradient-to-r")
  })

  it("uses the shared attached folder branch picker treatment for all chat inputs", () => {
    expect(source).not.toContain("attachFolderBranchPickerToInput")
    expect(conversationShellSource).not.toContain(
      "attachFolderBranchPickerToInput"
    )
    expect(messageInputSource).not.toContain("attachFolderBranchPickerToInput")
    expect(messageInputSource).toContain(
      "const folderBranchPickerAttached = hasFolderBranchPicker"
    )
    expect(messageInputSource).not.toContain("rounded-b-none")

    const pickerStart = messageInputSource.indexOf(
      "{hasFolderBranchPicker && ("
    )
    const pickerEnd = messageInputSource.indexOf(
      "<ImagePreviewDialog",
      pickerStart
    )
    expect(pickerStart).toBeGreaterThan(-1)
    expect(pickerEnd).toBeGreaterThan(pickerStart)

    const pickerWrapper = messageInputSource.slice(pickerStart, pickerEnd)
    expect(messageInputSource).toContain(
      '"overflow-hidden rounded-xl transition-colors"'
    )
    expect(messageInputSource).not.toContain("bg-muted/60")
    expect(messageInputSource).toContain(': "contents"')
    expect(messageInputSource).toContain(
      '"rounded-xl border border-input bg-background focus-within:border-ring focus-within:ring-[3px] focus-within:ring-inset focus-within:ring-ring/50"'
    )
    expect(pickerWrapper).not.toContain("border-t border-input")
    expect(pickerWrapper).not.toContain("bg-muted/30")
    expect(pickerWrapper).toContain("pt-1")
    expect(pickerWrapper).not.toContain("py-1")
    expect(pickerWrapper).toContain("rounded-b-xl")
    expect(pickerWrapper).toContain("mt-1.5")
    expect(pickerWrapper).toContain("pl-2")
    expect(pickerWrapper).not.toContain("pl-[")
    expect(pickerWrapper).not.toContain("pl-1.5")
    expect(pickerWrapper).not.toMatch(/\bborder-b\b/)
    expect(pickerWrapper).not.toMatch(/\bborder-x\b/)
  })

  it("keeps ordinary chat input constrained to the message column width", () => {
    expect(conversationShellSource).toContain(
      'className="mx-auto w-full max-w-3xl"'
    )
    expect(chatInputSource).toContain('className="px-4 pt-0 pb-1"')
    expect(chatInputSource).toContain('className="min-h-24 max-h-60"')
    expect(chatInputSource).not.toContain("containerClassName")
    expect(source).not.toContain("containerClassName")
    expect(conversationShellSource).not.toContain("containerClassName")
    expect(source).toContain("mx-auto flex w-full max-w-2xl")
  })
})
