---
name: secure-secrets
description: Request credentials through the secure browser dialog and use opaque references through trusted service operations.
---

Use the `build-your-own-secrets` MCP tools whenever a credential is needed. For general named credentials, call `request_secret` with `service: "opaque"`; encrypted storage/status/revocation is supported, without a plaintext resolver. Only registered trusted adapters may use stored references.

1. Call `request_secret` with `name: "GITHUB_TOKEN"` and `service: "github"`. Do not ask the user to paste a token into chat, an ordinary question answer, a file, or a command.
2. The secure dialog collects the value separately. The tool returns only an opaque `id` and `state`. If pending, finish the current turn and tell the user to continue after saving the credential; do not poll repeatedly.
3. Call `get_secret_status` with `reference: <id>` when the user continues.
4. Call `github_identity` with that reference to verify the GitHub account. The trusted platform performs a fixed read-only GitHub operation and returns only login/account id.

Never attempt to resolve a reference, read the vault files, fetch or print secret values, or insert credentials into prompts, generated source, environment files, tool arguments, output, logs, or documents. Treat references as conversation-scoped handles; do not reuse them in other conversations.

Generic secret storage is supported. GitHub account verification is currently the only trusted-use adapter. Other providers, generated-app secret environment injection, arbitrary HTTP calls, and token substitution into model output are unsupported. Ask the user to use the relevant existing connector when available instead of suggesting unsafe token handling.
