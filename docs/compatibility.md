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

The local CI gate runs `scripts/validate_compatibility.py` before Rust checks.
It verifies the schema, allowed statuses, ISO review date, repository evidence
paths, and the Git ignore rule for `base/a-shell`. This is evidence hygiene; it
does not execute a-Shell or upgrade any comparison state by itself.

## Differential scenario runner

[`compat/scenarios/core.json`](../compat/scenarios/core.json) is a versioned
scenario document. Each scenario contains one bounded command, optional UTF-8
stdin, and an explicit list of normalization rules. The initial set covers
quoted arguments, pipelines, command substitution, conditional status,
confined redirection, working-directory state, stderr/status, and alias
bypass.

Validate only the scenario contract with:

```bash
python3 scripts/compatibility_runner.py --validate-only
```

After building the CLI, execute the Rune side with:

```bash
cargo build -p rune-cli
python3 scripts/compatibility_runner.py > /tmp/rune-compatibility.json
```

Every scenario gets a fresh temporary filesystem. The output records Rune
stdout, stderr, and process status, but labels the comparison `pending` because
this workspace cannot execute the Apple reference. A separate Apple/a-Shell
harness may place files named after the scenario (with `/` replaced by `__`)
in an observation directory and then run:

```bash
python3 scripts/compatibility_runner.py --reference-dir PATH
```

An observation must identify `a-Shell`, carry the matching scenario id, and
contain captured stdout, stderr, and an exit status. The runner compares only
the declared normalized streams and status; it does not inspect or execute
`base/a-shell`. A mismatch is a useful regression lead, not permission to mark
the matrix `supported` without reviewing the reference capture and preserving
the scenario-specific regression test.

The local CI runs the schema validator and runner unit tests, not the
comparison mode. This keeps the absence of an Apple runtime visible instead of
turning a self-comparison or a missing observation into compatibility evidence.

For every differential test, record the scenario, observed reference behavior,
Rune behavior, normalization rules, and the regression test that preserves the
result. Keep compatibility claims bounded to the tested scenario.
