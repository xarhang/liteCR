# Synthetic MCP stdio harness

This isolated test package checks MCP framing with `rmcp 3.5.1` and a fixed synthetic demo tool. It is not part of the liteCR Cargo workspace, application binary, or release archive. It has no database, network client, credential input, patient fixture, or registry integration. Do not configure a production AI client to access real data through this harness.

Rust 1.88 is required by the pinned SDK. From the repository root, run it in WSL or another supported Rust 1.88 environment:

```sh
cd tests/mcp-harness
cargo +1.88.0 test --locked
```

The 64 KiB newline-delimited input ceiling belongs only to this prototype. The reader wrapper exists because the pinned SDK's stdio transport reads complete lines before parsing; it is not the selected production wire limit. An oversized line closes the synthetic process, and the regression confirms a valid call after that line is never served. Process tests cover the initialize-based lifecycle (`2025-11-25`) and the inline lifecycle (`2026-07-28`, with required client context on every request), rejection of missing context before and after the inline lifecycle starts, recovery for a valid request, one fixed demo tool, JSON-only stdout, and over-limit input rejection. A negative-surface regression also checks that resources, resource templates and prompts are unavailable or empty, reads/gets and unknown tools are rejected, and unsupported methods return method-not-found. These direct JSON-RPC tests do not establish compatibility with a named AI host or make either revision a production support commitment.

This test client sends protocol messages directly and is not a compatibility or usability test for a named AI application. No supported-client configuration has been published. The harness proves neither application authorization nor real-data audit, consent, scope, or disclosure controls; those are not implemented by this harness.
