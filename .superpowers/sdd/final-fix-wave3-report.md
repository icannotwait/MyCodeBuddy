# Final branch review — wave3 fix report

**Branch:** `feat/popout-close-acp-keepalive`  
**Status:** P1 residual gen overwrite fixed  
**Date:** 2026-07-25

## P1 issue

**Bug:** `close_reserved_outcome_after_residual` always preferred residual `max_gen` whenever present, including when primary reverse already returned `Reversed { gen }`. Residual `max_gen` is an aggregate across connections; generations are per-connection. Overwriting a successful primary reverse gen with residual max can publish the wrong lease generation to FE.

## Rule (fixed)

1. If primary reverse returned `Reversed { gen }`, **keep that gen** for published outcome / lease.
2. Residual recovery may only **upgrade** non-reclaimable outcomes (`ConnectionGone` / `ReverseUncertain` / `Superseded`) to `Reversed` using residual gen.
3. **Never** overwrite a successful primary `Reversed` gen with aggregate residual `max_gen`.

## Fix

1. **`close_reserved_outcome_after_residual`** (~line 113): match primary outcome — keep `Reversed`; upgrade only `ConnectionGone` / `ReverseUncertain` / `Superseded` when residual gen is `Some`.
2. **Main close residual path:** select via the same helper; commit only when residual upgrades the primary outcome (avoids residual max clobbering primary success before publish).
3. **Test** `close_reserved_outcome_prefers_residual_reversed_over_connection_gone`: removed unsafe “residual wins over primary Reversed” assertion; assert primary `Reversed{3}` + residual `9` stays `Reversed{3}`; added Superseded upgrade + residual-None keep cases.

**Files:**
- `src-tauri/src/commands/conversation_popout.rs`
- `.superpowers/sdd/final-fix-wave3-report.md`

## Verification

```text
cargo test --features test-utils --lib conversation_popout
# 52 passed; 0 failed
```

## Commit

- `26e5b9ea` — `fix(popout): keep primary Reversed gen over residual max_gen`
