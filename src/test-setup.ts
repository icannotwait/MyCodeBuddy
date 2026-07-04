import "@testing-library/jest-dom/vitest"

const hasStorageMethods = (storage: Storage | undefined): storage is Storage =>
  typeof storage?.getItem === "function" &&
  typeof storage.setItem === "function" &&
  typeof storage.removeItem === "function" &&
  typeof storage.clear === "function"

const getWindowLocalStorage = (): Storage | undefined => {
  try {
    return typeof window !== "undefined" ? window.localStorage : undefined
  } catch {
    return undefined
  }
}

const createMemoryStorage = (): Storage => {
  const entriesByStorage = new WeakMap<object, Map<string, string>>()
  const entriesFor = (storage: object) => {
    let entries = entriesByStorage.get(storage)
    if (!entries) {
      entries = new Map<string, string>()
      entriesByStorage.set(storage, entries)
    }
    return entries
  }

  const memoryPrototype =
    typeof Storage !== "undefined" ? Storage.prototype : {}

  Object.defineProperties(memoryPrototype, {
    length: {
      configurable: true,
      get() {
        return entriesFor(this).size
      },
    },
    clear: {
      configurable: true,
      value() {
        entriesFor(this).clear()
      },
    },
    getItem: {
      configurable: true,
      value(key: string) {
        return entriesFor(this).get(String(key)) ?? null
      },
    },
    key: {
      configurable: true,
      value(index: number) {
        return Array.from(entriesFor(this).keys())[index] ?? null
      },
    },
    removeItem: {
      configurable: true,
      value(key: string) {
        entriesFor(this).delete(String(key))
      },
    },
    setItem: {
      configurable: true,
      value(key: string, value: string) {
        entriesFor(this).set(String(key), String(value))
      },
    },
  })

  return Object.create(memoryPrototype) as Storage
}

// Node 25 can expose an incomplete global localStorage object when launched
// with a malformed --localstorage-file flag. Prefer jsdom's implementation in
// tests, or a minimal in-memory fallback, so bare `localStorage.*` calls behave
// like browser code.
const jsdomLocalStorage = getWindowLocalStorage()
const testLocalStorage = hasStorageMethods(jsdomLocalStorage)
  ? jsdomLocalStorage
  : createMemoryStorage()
if (
  typeof globalThis !== "undefined" &&
  !hasStorageMethods(globalThis.localStorage as Storage | undefined)
) {
  Object.defineProperty(globalThis, "localStorage", {
    configurable: true,
    enumerable: true,
    writable: true,
    value: testLocalStorage,
  })
}
if (
  typeof window !== "undefined" &&
  !hasStorageMethods(window.localStorage as Storage | undefined)
) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    enumerable: true,
    writable: true,
    value: testLocalStorage,
  })
}

// jsdom doesn't implement a few layout APIs that ProseMirror's EditorView
// touches on mount (used by Tiptap-based editors such as the message composer).
// Polyfill them as no-ops so headless/component editor tests can construct a
// view. Only defined when missing, so real browsers/environments are untouched.
if (typeof document !== "undefined" && !document.elementFromPoint) {
  document.elementFromPoint = () => null
}
if (typeof Element !== "undefined") {
  // jsdom doesn't implement scrollIntoView; the composer's suggestion popup
  // calls it to keep the active row visible.
  Element.prototype.scrollIntoView ??= () => {}
  // jsdom doesn't implement Pointer Capture; Radix menus/popovers touch these
  // during the pointer interactions @testing-library/user-event drives.
  Element.prototype.hasPointerCapture ??= () => false
  Element.prototype.setPointerCapture ??= () => {}
  Element.prototype.releasePointerCapture ??= () => {}
}
if (typeof globalThis !== "undefined" && !("ResizeObserver" in globalThis)) {
  // jsdom doesn't implement ResizeObserver; cmdk (the command palette used by
  // the branch/folder pickers) constructs one on mount. A no-op stub is enough
  // for headless rendering — layout callbacks never need to fire.
  ;(globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
}
if (typeof Range !== "undefined") {
  Range.prototype.getClientRects ??= () =>
    ({
      length: 0,
      item: () => null,
      [Symbol.iterator]: function* () {},
    }) as unknown as DOMRectList
  Range.prototype.getBoundingClientRect ??= () =>
    ({
      top: 0,
      left: 0,
      right: 0,
      bottom: 0,
      width: 0,
      height: 0,
      x: 0,
      y: 0,
    }) as DOMRect
}
