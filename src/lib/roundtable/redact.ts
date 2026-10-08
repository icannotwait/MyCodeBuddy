/** Strip local paths, hidden reasoning markers, and token-shaped secrets. */
export function redactRoundtableText(text: string): string {
  return text
    .replace(/[A-Za-z]:\\[^\s]+/g, "")
    .replace(/思考过程/g, "")
    .replace(/sk-[a-z0-9-]+/gi, "[redacted]")
}
