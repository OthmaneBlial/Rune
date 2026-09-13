# Compatibility policy

Rune uses the local a-Shell checkout as a behavioral reference. The reference
has a BSD 3-Clause license; Rune does not copy its source, assets, or UI.

The machine-readable matrix in
[`compat/a-shell-compatibility.json`](../compat/a-shell-compatibility.json)
starts every area as `planned`. A command or subsystem can move to
`supported` only when Rune behavior is implemented, normal and error paths are
covered by tests, and any relevant differences are documented. A working API
surface or a similarly named command is not evidence of compatibility.

For differential tests, record the scenario, observed reference behavior,
Rune behavior, normalization rules, and the regression test that preserves the
result. Keep compatibility claims bounded to the tested scenario.
