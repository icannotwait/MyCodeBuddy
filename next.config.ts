import type { NextConfig } from "next"
import createNextIntlPlugin from "next-intl/plugin"
import path from "node:path"
import { fileURLToPath } from "node:url"
import packageJson from "./package.json"

const isProd = process.env.NODE_ENV === "production"
const internalHost = process.env.TAURI_DEV_HOST || "localhost"
const configDir = path.dirname(fileURLToPath(import.meta.url))
const withNextIntl = createNextIntlPlugin({
  requestConfig: "./src/i18n/request.ts",
  experimental: {
    messages: {
      path: "./src/i18n/messages",
      format: "json",
      locales: [
        "en",
        "zh-CN",
        "zh-TW",
        "ja",
        "ko",
        "es",
        "de",
        "fr",
        "pt",
        "ar",
      ],
      precompile: true,
    },
  },
})

const nextConfig: NextConfig = {
  output: "export",
  // Worktree sits under D:\MyCodeBuddy which also has a pnpm-lock.yaml;
  // pin turbopack root so typecheck/build do not resolve the parent tree.
  turbopack: {
    root: configDir,
  },
  // The build type-checks tsconfig.json minus the test files; that file says
  // why.
  typescript: {
    tsconfigPath: "tsconfig.build.json",
  },
  // Development-only pages (`page.gallery.tsx`, e.g. the activity timeline
  // gallery) are routes only when CODEG_GALLERY=1, so release builds never
  // ship them.
  ...(process.env.CODEG_GALLERY === "1"
    ? { pageExtensions: ["gallery.tsx", "tsx", "ts", "jsx", "js"] }
    : {}),
  env: {
    // The service worker's caches are keyed to the release; see
    // serviceWorkerUrl() in src/lib/service-worker.ts.
    CODEG_APP_VERSION: packageJson.version,
  },
  images: {
    unoptimized: true,
  },
  assetPrefix: isProd ? undefined : `http://${internalHost}:3000`,
}

export default withNextIntl(nextConfig)
