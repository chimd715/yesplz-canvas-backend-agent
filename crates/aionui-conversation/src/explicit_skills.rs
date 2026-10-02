//! Deterministic dollar-skill admission and trusted instruction loading.
use std::collections::BTreeSet;

use crate::ConversationError;
use crate::skill_resolver::SkillResolver;

/// Match the renderer's prose syntax. Escaped dollars and backtick code are literal.
pub(crate) fn names_from_content(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut names = Vec::new();
    let mut code_ticks = 0;
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '\\' {
            index += 2;
            continue;
        }
        if current == '`' {
            let count = chars[index..].iter().take_while(|&&value| value == '`').count();
            if code_ticks == 0 {
                code_ticks = count;
            } else if code_ticks == count {
                code_ticks = 0;
            }
            index += count;
            continue;
        }
        let boundary = index == 0 || chars[index - 1].is_whitespace() || "([{'\"".contains(chars[index - 1]);
        if code_ticks == 0 && current == '$' && boundary && chars.get(index + 1).is_some_and(char::is_ascii_alphabetic)
        {
            let mut end = index + 1;
            while chars
                .get(end)
                .is_some_and(|value| value.is_ascii_alphanumeric() || *value == '_' || *value == '-')
            {
                end += 1;
            }
            let name: String = chars[index + 1..end].iter().collect();
            if !names.contains(&name) {
                names.push(name);
            }
            index = end;
        } else {
            index += 1;
        }
    }
    names
}

pub(crate) fn requested_names(content: &str, injected: &[String]) -> Result<Vec<String>, ConversationError> {
    let mut names = names_from_content(content);
    for name in injected {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    if names.len() > 16
        || names.iter().any(|name| {
            name.len() > 64
                || name.is_empty()
                || !name.as_bytes()[0].is_ascii_alphabetic()
                || !name
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || value == b'_' || value == b'-')
        })
    {
        return Err(ConversationError::bad_request("Invalid explicit skill reference"));
    }
    Ok(names)
}

/// Full bodies come from the authenticated user's resolver, never browser payloads.
/// The caller uses this only for agent input, preserving ordinary history text.
pub(crate) async fn instruction_prefix(
    resolver: &dyn SkillResolver,
    user_id: &str,
    names: &[String],
) -> Result<String, ConversationError> {
    if names.is_empty() {
        return Ok(String::new());
    }
    let loaded = resolver.load_skill_bodies_for_user(user_id, names).await;
    let available: BTreeSet<&str> = loaded
        .iter()
        .filter(|skill| !skill.body.trim().is_empty())
        .map(|skill| skill.name.as_str())
        .collect();
    if names.iter().any(|name| !available.contains(name.as_str())) {
        return Err(ConversationError::bad_request(
            "An explicitly requested skill is unavailable",
        ));
    }
    let instructions: Vec<_> = names.iter().filter_map(|name| loaded.iter().find(|skill| skill.name == *name))
        .map(|skill| serde_json::json!({ "name": skill.name, "source_directory": skill.source_path.to_string_lossy(), "instructions": skill.body })).collect();
    Ok(format!(
        "Apply these explicitly selected skills for this turn. Resolve relative resources from each source directory.\n\n{}\n\nUser request:\n",
        serde_json::to_string(&instructions).expect("skill instruction values serialize")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_resolver::{LoadedAgentSkill, ResolvedAgentSkill};
    use async_trait::async_trait;

    #[test]
    fn explicit_syntax_matches_browser_prose_and_ignores_literals() {
        assert_eq!(
            names_from_content(
                "Use $report then \"$build-doc\" and $report. $42 cost email$bad \\$literal `$code`\n```sh\n$hidden\n```\n$final_skill"
            ),
            vec!["report", "build-doc", "final_skill"]
        );
    }

    #[test]
    fn requested_names_are_exact_deduplicated_and_bounded() {
        assert_eq!(
            requested_names("$report", &["report".into(), "review".into()]).unwrap(),
            vec!["report", "review"]
        );
        assert!(requested_names("plain", &["../foreign".into()]).is_err());
        assert!(
            requested_names(
                "plain",
                &(0..17).map(|index| format!("skill{index}")).collect::<Vec<_>>()
            )
            .is_err()
        );
    }

    struct Resolver;
    #[async_trait]
    impl SkillResolver for Resolver {
        async fn auto_inject_names(&self) -> Vec<String> {
            Vec::new()
        }
        async fn resolve_skills(&self, _: &[String]) -> Vec<ResolvedAgentSkill> {
            Vec::new()
        }
        async fn load_skill_bodies_for_user(&self, user_id: &str, names: &[String]) -> Vec<LoadedAgentSkill> {
            if user_id != "alice" || !names.iter().any(|name| name == "report") {
                return Vec::new();
            }
            vec![LoadedAgentSkill {
                name: "report".into(),
                body: "Use exact report instructions.".into(),
                source_path: "/trusted/alice/report".into(),
            }]
        }
    }

    #[tokio::test]
    async fn uses_exact_authenticated_skill_body_and_source_root() {
        let prefix = instruction_prefix(&Resolver, "alice", &["report".into()])
            .await
            .unwrap();
        assert!(prefix.contains("Use exact report instructions."));
        assert!(prefix.contains("/trusted/alice/report"));
        assert!(
            instruction_prefix(&Resolver, "alice", &["reports".into()])
                .await
                .is_err()
        );
        assert!(instruction_prefix(&Resolver, "bob", &["report".into()]).await.is_err());
    }
}
