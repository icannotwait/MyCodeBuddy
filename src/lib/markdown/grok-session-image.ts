export type GrokSessionImageExtension = "png" | "jpg" | "jpeg" | "webp" | "gif"

export type GrokSessionImageRef = {
  path: string
  filename: string
  extension: GrokSessionImageExtension
}

export const GROK_SESSION_IMAGE_MIME_BY_EXTENSION = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  webp: "image/webp",
  gif: "image/gif",
} as const satisfies Record<GrokSessionImageExtension, string>

const SCHEME_PREFIX = /^[A-Za-z][A-Za-z0-9+.-]*:/
const DRIVE_PREFIX = /^[A-Za-z]:/
const PORTABLE_INVALID = /[<>:"|?*]/
const DEVICE_STEM = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$/i
const UTF8_ENCODER = new TextEncoder()

function isGrokSessionImageExtension(
  value: string
): value is GrokSessionImageExtension {
  return Object.prototype.hasOwnProperty.call(
    GROK_SESSION_IMAGE_MIME_BY_EXTENSION,
    value
  )
}

function utf8Length(value: string): number {
  return UTF8_ENCODER.encode(value).length
}

function containsControl(value: string): boolean {
  for (const char of value) {
    const code = char.codePointAt(0)!
    if (code <= 0x1f || code === 0x7f) return true
  }
  return false
}

function invalidFilename(filename: string): boolean {
  if (
    !filename ||
    filename.startsWith(" ") ||
    filename.endsWith(" ") ||
    containsControl(filename) ||
    PORTABLE_INVALID.test(filename) ||
    filename.includes("#") ||
    utf8Length(filename) > 255
  ) {
    return true
  }

  const firstDot = filename.indexOf(".")
  const lastDot = filename.lastIndexOf(".")
  if (lastDot <= 0 || lastDot === filename.length - 1) return true

  return DEVICE_STEM.test(filename.slice(0, firstDot))
}

export function parseGrokSessionImageRef(
  raw: string
): GrokSessionImageRef | null {
  if (utf8Length(raw) > 1024 || containsControl(raw)) return null

  const trimmed = raw.replace(/^ +| +$/g, "")
  if (!trimmed || SCHEME_PREFIX.test(trimmed) || DRIVE_PREFIX.test(trimmed)) {
    return null
  }

  const queryOrFragment = trimmed.search(/[?#]/)
  const pathPart =
    queryOrFragment === -1 ? trimmed : trimmed.slice(0, queryOrFragment)

  let decoded: string
  try {
    decoded = decodeURIComponent(pathPart)
  } catch {
    return null
  }

  if (
    !decoded ||
    decoded.includes("\\") ||
    decoded.split("/").some((component) => component === "..")
  ) {
    return null
  }

  let filename: string
  if (decoded.startsWith("images/")) {
    filename = decoded.slice("images/".length)
  } else if (decoded.startsWith("./images/")) {
    filename = decoded.slice("./images/".length)
  } else {
    return null
  }

  if (filename.includes("/") || invalidFilename(filename)) return null

  const extension = filename.slice(filename.lastIndexOf(".") + 1).toLowerCase()
  if (!isGrokSessionImageExtension(extension)) return null

  return {
    path: `images/${filename}`,
    filename,
    extension,
  }
}
