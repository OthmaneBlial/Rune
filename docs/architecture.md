# Rune architecture

## Direction

Rune is being built as an independent Rust-native terminal core with a native
Apple frontend. The core must be useful and testable without UIKit or SwiftUI;
the frontend will translate user input and streamed command events across a
narrow, versioned boundary.

The local a-Shell checkout is used to understand user-visible behavior and iOS
constraints. It is not a source dependency or an implementation template.

## Initial crate boundaries

- `rune-shell`: lexical and syntactic shell concerns. It produces explicit
  execution plans rather than executing commands.
- `rune-fs`: path resolution and filesystem policy. Commands receive this
  abstraction rather than reaching into Apple APIs directly.
- `rune-core`: command registry, command context, session state, and execution
  results.
- `rune-ffi`: a deliberately narrow C ABI for opaque session handles and owned
  stdout/stderr buffers. Its unsafe code is isolated at the boundary.
- `apps/rune-cli`: a small host executable used for local development and
  end-to-end checks. It is not the iOS frontend.

These boundaries are deliberately small. New crates should be added only when
they own a coherent capability with tests.

## Runtime direction

The first execution engine is synchronous and deterministic so behavior can be
tested easily. Its result model already separates stdout, stderr, and exit
status. Later async execution and streaming can be introduced behind the same
conceptual event boundary once iOS cancellation and rendering requirements are
specified.

The filesystem starts with a host-backed root for local development. The root
is a policy boundary: paths are resolved relative to it, `~` maps to the root,
and traversal outside the root is rejected. An Apple adapter will map that
root to the app's Documents directory and add user-approved external roots
through security-scoped bookmarks.

Apple source integration will be added without installing a new Xcode or
simulator footprint. Until an existing Apple toolchain is explicitly used,
Swift compilation and runtime behavior remain unverified gates rather than
assumed capabilities.
