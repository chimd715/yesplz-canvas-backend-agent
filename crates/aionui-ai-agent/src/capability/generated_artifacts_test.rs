use super::*;

#[tokio::test]
async fn generated_file_validator_accepts_documents_and_rejects_unsafe_paths() {
    let workspace = tempfile::tempdir().unwrap();
    tokio::fs::create_dir(workspace.path().join("reports")).await.unwrap();
    for extension in EXTENSIONS {
        let file = format!("reports/output.{extension}");
        tokio::fs::write(workspace.path().join(&file), "fixture").await.unwrap();
        let (_, relative) = validate_generated_file(workspace.path(), &file).await.unwrap();
        assert_eq!(relative, file);
    }
    for path in [
        "../outside.html",
        "/etc/secret.html",
        "missing.html",
        "output.exe",
        "output\n.html",
        "",
    ] {
        assert!(
            validate_generated_file(workspace.path(), path).await.is_err(),
            "{path:?}"
        );
    }
    tokio::fs::create_dir(workspace.path().join("directory.html"))
        .await
        .unwrap();
    assert_eq!(
        validate_generated_file(workspace.path(), "directory.html")
            .await
            .unwrap_err(),
        "Generated output must be a regular file"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn generated_file_validator_rejects_symlink_escapes_and_nonregular_files() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("private.html"), "private").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("private.html"),
        workspace.path().join("escape.html"),
    )
    .unwrap();
    assert_eq!(
        validate_generated_file(workspace.path(), "escape.html")
            .await
            .unwrap_err(),
        "Generated files must remain inside the workspace"
    );
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("nested")).unwrap();
    assert!(
        validate_generated_file(workspace.path(), "nested/private.html")
            .await
            .is_err()
    );
    let socket_path = workspace.path().join("socket.html");
    let _socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    assert_eq!(
        validate_generated_file(workspace.path(), "socket.html")
            .await
            .unwrap_err(),
        "Generated output must be a regular file"
    );
}

#[tokio::test]
async fn publish_tool_is_non_deferred_and_never_echoes_untrusted_paths_in_errors() {
    let directory = tempfile::tempdir().unwrap();
    let tool = PublishArtifactTool(directory.path().into());
    assert_eq!(tool.name(), PUBLISH_ARTIFACT_TOOL);
    assert!(!tool.is_deferred());
    std::fs::write(directory.path().join("report.md"), "# Report").unwrap();
    assert!(!tool.execute(json!({"file_path":"report.md"})).await.is_error);
    for input in [
        json!({"file_path":"/private/TOKEN.html"}),
        json!({"file_path":"report.md", "scope":"foreign"}),
        json!({"file_path":123}),
    ] {
        let result = tool.execute(input).await;
        assert!(result.is_error);
        assert!(!result.content.contains("TOKEN"));
    }
}
