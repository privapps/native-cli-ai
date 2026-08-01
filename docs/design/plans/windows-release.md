# Windows Release Compatibility Plan

**Type:** Implementation plan
**Status:** Partially verified; native Windows execution is covered by CI and remains pending a successful runner result
**Date:** Unknown (legacy document)
**Related:** None
**Last verified:** 2026-07-31 (documentation review)

1. Audit the workspace for Unix-only networking, process, and filesystem assumptions.
2. Keep Unix domain sockets on Unix and use collision-safe loopback TCP IPC on Windows behind target-specific implementations.
3. Preserve the public IPC API so CLI attach, serve, and ordinary interactive/run flows compile unchanged.
4. Use platform-native shells and process termination, safe runtime-directory fallbacks, and strict release gating for supported tags.
5. Verify formatting, linting, tests, the native release build, and a Windows target check where the local toolchain permits.
6. Run the repository's `windows-runtime` CI job on `windows-latest` for native process, PTY, IPC, and release smoke acceptance.
7. Review the diff and report verification results. Repository policy prohibits creating branches, staging files, committing, pushing, or opening a PR as part of this work.
