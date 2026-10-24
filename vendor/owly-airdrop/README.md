# Owly AirDrop source

Pinned source: `https://github.com/t4t5/owly`, v0.4.0,
`2e9b2b0228432d5e402a9c90a031b419960d0bd1`. The source, upstream tests and MIT
license are retained here; the standalone Cargo manifest disables workspace
inheritance. `UPSTREAM-README.md` contains the original crate overview.

The Windows integration uses these protocol modules and state machines through
`wayline-core`, which owns application callbacks, cancellation, board feedback,
identity storage and network workers. There is one HTTP/plist/archive/mDNS
implementation, in this crate.

Local adaptations validate compression and HTTP completion before publishing
files, expose bounded upload progress and optional validated DVZip completion,
support cooperative archive preparation cancellation, and use atomic Windows
no-replace publication with rollback on a failed commit or final response.
Accepted responses requesting connection closure reconnect before upload;
transport failure/timeout remains distinct from an explicit refusal. Windows
regressions live in `crates/wayline-core/tests/airdrop_migration.rs`; upstream
library/vector tests run on Windows, while the veth harness requires Linux.
Keep source attribution and the MIT license in release packages.
