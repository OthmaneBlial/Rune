# Performance notes

Rune keeps performance work evidence-led. The portable CLI and Rust core are
measured locally; no host timing is presented as an iPhone or iPad benchmark.

## Reproduce the baseline

From the repository root:

```bash
./scripts/bench.sh
```

The script builds the CLI first, then measures three warm-process workflows
inside a temporary confined VFS root. It reports `real`, `user`, and `sys`
time and removes only that temporary root.

## Latest local sample

Captured on 2026-09-14 with macOS 26.6, Apple arm64, Rust 1.95.0:

| Workflow | Real time |
| --- | ---: |
| CLI startup (`true`) | 1.75 s |
| Confined filesystem pipeline | 0.01 s |
| Bounded `tree` rendering | 0.01 s |

The startup sample includes process launch and session initialization but not
the preceding compilation step. The filesystem and tree samples use the same
temporary root and are small smoke workloads, not throughput limits.

These values are a dated developer-machine baseline, not a performance
guarantee or a release gate. Apple launch latency, memory use, scrolling
smoothness, and large-output frame time remain unverified until the native
runtime can be built and measured on the target hardware.
