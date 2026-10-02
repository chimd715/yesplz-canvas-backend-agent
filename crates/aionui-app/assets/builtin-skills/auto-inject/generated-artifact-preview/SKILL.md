---
name: generated-artifact-preview
description: Show completed workspace HTML and documents immediately in the customer's preview using trusted file tools.
---

When creating an HTML page or a document, save the complete file inside the current conversation's workspace. Successful native `Write` calls automatically open supported files in the customer's preview; do not invent a preview URL or put a server filesystem path in the chat as a substitute.

For files created with shell or Office tools, call `PublishArtifact` with `file_path` set to the workspace-relative file name after the operation succeeds. It accepts regular HTML/HTM, Markdown, TXT, CSV, PDF, DOCX, XLSX and PPTX files. Failed writes, missing files, unsupported formats and paths outside the workspace are not published. You cannot choose a different user, conversation or project.

Generated HTML document preview is static and isolated: scripts, forms, navigation, external resources and network requests are disabled. Include CSS inline and images as data URLs when needed. Use the separate existing app/browser preview for interactive applications. Markdown/text and PDF use the existing document viewers; Office document rendering depends on the installed Office preview tool. Do not claim unsupported legacy or macro-enabled Office formats work.

Never publish tokens, passwords, credential files or secret values. Request credentials through the secure credential dialog and retain only opaque references. Do not read the vault or include credentials in generated documents or source.
