import { afterEach, beforeAll, describe, expect, it, vi } from "vitest"

import {
  createPetSpriteObjectUrl,
  revokePetSpriteObjectUrl,
} from "@/lib/pet/sprite-url"

// jsdom does not implement the URL object-URL helpers; provide stubs so the
// tests can spy on them.
beforeAll(() => {
  if (typeof URL.createObjectURL !== "function") {
    URL.createObjectURL = () => "blob:stub"
  }
  if (typeof URL.revokeObjectURL !== "function") {
    URL.revokeObjectURL = () => {}
  }
})

describe("createPetSpriteObjectUrl", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("decodes base64 into a Blob and returns an object URL", () => {
    const createObjectURL = vi
      .spyOn(URL, "createObjectURL")
      .mockReturnValue("blob:pet-1")
    // "hi" base64-encoded.
    const url = createPetSpriteObjectUrl({
      mime: "image/webp",
      dataBase64: btoa("hi"),
    })

    expect(url).toBe("blob:pet-1")
    expect(createObjectURL).toHaveBeenCalledTimes(1)
    const blob = createObjectURL.mock.calls[0][0] as Blob
    expect(blob).toBeInstanceOf(Blob)
    expect(blob.type).toBe("image/webp")
    expect(blob.size).toBe(2)
  })

  it("sizes the Blob to the decoded byte length", () => {
    let captured: Blob | null = null
    vi.spyOn(URL, "createObjectURL").mockImplementation((blob) => {
      captured = blob as Blob
      return "blob:x"
    })

    createPetSpriteObjectUrl({ mime: "image/png", dataBase64: btoa("ABCDE") })

    expect(captured!.size).toBe(5)
  })
})

describe("revokePetSpriteObjectUrl", () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it("revokes blob: URLs", () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {})

    revokePetSpriteObjectUrl("blob:pet-1")

    expect(revoke).toHaveBeenCalledWith("blob:pet-1")
  })

  it("ignores non-blob URLs", () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {})

    revokePetSpriteObjectUrl("https://example.com/x.webp")

    expect(revoke).not.toHaveBeenCalled()
  })

  it("ignores null and undefined", () => {
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => {})

    revokePetSpriteObjectUrl(null)
    revokePetSpriteObjectUrl(undefined)

    expect(revoke).not.toHaveBeenCalled()
  })
})
