import base from "../../../vitest.config"

export default {
  ...base,
  test: {
    ...base.test,
    include: ["docs/reviews/reply-loss-audit/repro.test.ts"],
  },
}
