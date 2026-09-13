//! Filesystem policy and path resolution for Rune.
//!
//! The first concrete filesystem implementation is introduced with the first
//! command-execution slice. Keeping this crate separate ensures command logic
//! does not depend directly on `UIKit` or platform-specific filesystem APIs.
