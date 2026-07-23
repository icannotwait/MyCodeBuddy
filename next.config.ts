import type { NextConfig } from "next"
import createNextIntlPlugin from "next-intl/plugin"
import path from "node:path"
import { fileURLToPath } from "node:url"

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
  images: {
    unoptimized: true,
  },
  assetPrefix: isProd ? undefined : `http://${internalHost}:3000`,
}

export default withNextIntl(nextConfig)
