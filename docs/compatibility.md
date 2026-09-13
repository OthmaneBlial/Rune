# Compatibility policy

Rune uses the local a-Shell checkout as a behavioral reference. The reference
has a BSD 3-Clause license; Rune does not copy its source, assets, or UI.

The machine-readable matrix in
[`compat/a-shell-compatibility.json`](../compat/a-shell-compatibility.json)
records both Rune's bounded implementation status and the state of its direct
comparison with a-Shell. `partial` means that Rune has a tested subset; it is
not a parity claim. An area can move to `supported` only when Rune behavior,
normal and error paths, and direct reference behavior are covered by regression
tests with documented normalization rules. A working API surface or a
similarly named command is not evidence of compatibility.

The current implemented areas remain `reference_comparison: pending` because
this workspace has no Apple runtime or a-Shell executable harness. The matrix
therefore exposes the real evidence paths without converting source inspection
or Rust-only tests into a false compatibility result.

For differential tests, record the scenario, observed reference behavior,
Rune behavior, normalization rules, and the regression test that preserves the
result. Keep compatibility claims bounded to the tested scenario.
