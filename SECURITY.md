# Security policy

Rune is designed around explicit capability boundaries, bounded inputs, and a
confined virtual filesystem. It is still an alpha source preview, and the
native Apple runtime has not been validated in this workspace.

## Scope

Please report issues involving:

- escaping the configured VFS root or bypassing path validation;
- unbounded input, output, archive, runtime, or persistence behavior;
- secrets or private values appearing in diagnostics, history, or serialized
  state despite the documented policy;
- unsafe FFI ownership, length, callback, or cancellation behavior;
- provider boundaries that accidentally grant ambient host access.

## Reporting

Do not open a public issue with an exploit, credential, private key, token,
personal path, host inventory, or real capture. Use GitHub's private
vulnerability-reporting option for this repository when it is available. If it
is not available, open a minimal public issue asking for a private reporting
channel without including sensitive details.

Include a minimal reproduction, the affected commit, platform/toolchain
information, and the expected versus observed boundary. Redact all sensitive
values before sharing.

## Supported release boundary

The current public release is a pre-1.0 source preview. Rust CLI behavior and
local tests are the strongest supported evidence. iOS linking, simulator/device
behavior, provider success, signing, and distribution are not supported claims
until they have been validated on their target environments.
