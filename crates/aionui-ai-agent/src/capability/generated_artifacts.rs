//! Workspace-only document publication; no content or host path leaves this validator.
use std::path::{Component, Path, PathBuf};

use aion_protocol::events::ToolCategory;
use aion_tools::Tool;
use aion_types::tool::{JsonSchema, ToolResult};
use serde_json::{Value, json};

pub const PUBLISH_ARTIFACT_TOOL: &str = "PublishArtifact";
const EXTENSIONS: &[&str] = &[
    "html", "htm", "md", "markdown", "mdown", "mkd", "txt", "csv", "pdf", "docx", "xlsx", "pptx",
];

/// Validate lexical and realpath containment immediately before publication.
/// Returns a project-relative name, never an unrestricted filesystem reference.
pub async fn validate_generated_file(workspace: &Path, file_path: &str) -> Result<(PathBuf, String), &'static str> {
    if file_path.is_empty() || file_path.len() > 4096 || file_path.chars().any(char::is_control) {
        return Err("Invalid generated file path");
    }
    let supplied = Path::new(file_path);
    #[cfg(unix)]
    if file_path.contains('\\') {
        return Err("Invalid generated file path");
    }
    if supplied.components().any(|part| matches!(part, Component::ParentDir)) {
        return Err("Generated files must remain inside the workspace");
    }
    let extension = supplied
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !EXTENSIONS.contains(&extension.as_str()) {
        return Err("Unsupported generated document format");
    }
    let root = tokio::fs::canonicalize(workspace)
        .await
        .map_err(|_| "Workspace is unavailable")?;
    let candidate = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        root.join(supplied)
    };
    if !candidate.starts_with(&root) {
        return Err("Generated files must remain inside the workspace");
    }
    let resolved = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|_| "Generated file is unavailable")?;
    if !resolved.starts_with(&root) {
        return Err("Generated files must remain inside the workspace");
    }
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|_| "Generated file is unavailable")?;
    if !metadata.is_file() {
        return Err("Generated output must be a regular file");
    }
    let relative = candidate
        .strip_prefix(&root)
        .map_err(|_| "Generated file is outside the workspace")?;
    let relative = relative
        .to_str()
        .ok_or("Invalid generated file path")?
        .replace('\\', "/");
    if relative.is_empty() {
        return Err("Generated output must be a file");
    }
    Ok((root, relative))
}

pub(crate) struct PublishArtifactTool(pub(crate) PathBuf);

#[async_trait::async_trait]
impl Tool for PublishArtifactTool {
    fn name(&self) -> &str {
        PUBLISH_ARTIFACT_TOOL
    }
    fn description(&self) -> &str {
        "Open a completed HTML, Markdown, text, CSV, PDF, DOCX, XLSX or PPTX document in the customer's preview. Use after creating a document with shell or office tools. Write already publishes supported files automatically. Only regular files inside this conversation's workspace are accepted; never publish credentials."
    }
    fn input_schema(&self) -> JsonSchema {
        json!({"type":"object","additionalProperties":false,"required":["file_path"],"properties":{"file_path":{"type":"string","minLength":1,"maxLength":4096,"description":"Workspace-relative path to the completed document"}}})
    }
    fn category(&self) -> ToolCategory {
        ToolCategory::Info
    }
    fn is_concurrency_safe(&self, _: &Value) -> bool {
        false
    }
    fn describe(&self, _: &Value) -> String {
        "Preview a generated document".into()
    }
    async fn execute(&self, input: Value) -> ToolResult {
        let result = match input.as_object() {
            Some(object) if object.len() == 1 => match object.get("file_path").and_then(Value::as_str) {
                Some(path) => validate_generated_file(&self.0, path).await.map(|_| ()),
                None => Err("Invalid generated file request"),
            },
            _ => Err("Invalid generated file request"),
        };
        match result {
            Ok(()) => ToolResult {
                content: "Document ready for owner-scoped preview.".into(),
                is_error: false,
            },
            Err(message) => ToolResult {
                content: message.into(),
                is_error: true,
            },
        }
    }
}

#[cfg(test)]
#[path = "generated_artifacts_test.rs"]
mod tests;
