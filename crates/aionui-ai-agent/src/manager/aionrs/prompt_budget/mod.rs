use std::sync::Arc;

use aion_providers::{LlmProvider, ProviderError};
use aion_types::llm::{LlmEvent, LlmRequest};
use async_trait::async_trait;
use tokio::sync::mpsc;

const OUTPUT_RESERVE: usize = 16_384;
const REQUEST_BYTE_LIMIT: usize = 1024 * 1024;

/// Service admission uses encoded bytes conservatively; this is not a tokenizer.
pub(super) fn context_limit(model: &str, configured: Option<usize>) -> Option<usize> {
    let catalog = match model.rsplit('/').next().unwrap_or(model) {
        "deepseek-v4.1-flash" => Some(1_000_000),
        "glm-5.3" => Some(262_144),
        "glm-5.3-flash" => Some(262_124),
        _ => None,
    };
    match (catalog, configured.filter(|value| *value > 0)) {
        (Some(catalog), Some(configured)) => Some(catalog.min(configured)),
        (catalog, configured) => catalog.or(configured),
    }
}

pub(super) struct BudgetedProvider {
    inner: Arc<dyn LlmProvider>,
    context: Option<usize>,
}

impl BudgetedProvider {
    pub(super) fn new(inner: Arc<dyn LlmProvider>, model: &str, configured: Option<usize>) -> Self {
        Self {
            inner,
            context: context_limit(model, configured),
        }
    }
}

#[async_trait]
impl LlmProvider for BudgetedProvider {
    async fn stream(&self, request: &LlmRequest) -> Result<mpsc::Receiver<LlmEvent>, ProviderError> {
        let Some(context) = self.context else {
            return self.inner.stream(request).await;
        };
        // This receives the engine's final request on EVERY iteration, including
        // resumed history, tool output, dynamically selected schemas and images.
        let tools: Vec<_> = request
            .tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name, "description": tool.description, "parameters": tool.input_schema,
                })
            })
            .collect();
        let encoded = serde_json::to_vec(&serde_json::json!({
            "model": request.model, "system": request.system, "messages": request.messages, "tools": tools,
            "thinking": format!("{:?}", request.thinking), "reasoning_effort": request.reasoning_effort,
        }))
        .map_err(|_| ProviderError::Parse("Could not measure model request".to_owned()))?;
        let reserve = OUTPUT_RESERVE.min(context / 4);
        let output = usize::try_from(request.max_tokens.unwrap_or(reserve as u32))
            .unwrap_or(usize::MAX)
            .max(reserve);
        if encoded.len().saturating_add(4096).saturating_add(output) > context
            || encoded.len().saturating_add(4096) > REQUEST_BYTE_LIMIT
        {
            return Err(ProviderError::PromptTooLong("Shorten the input or reduce conversation context; the assembled request exceeds the service model budget".to_owned()));
        }
        // An omitted provider cap must not consume the capacity reserved above.
        let mut bounded = request.clone();
        bounded.max_tokens = request.max_tokens.or(Some(reserve as u32));
        self.inner.stream(&bounded).await
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
