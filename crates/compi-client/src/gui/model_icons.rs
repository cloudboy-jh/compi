//! Model and provider marks for the chat's model picker: LobeHub's monochrome icons
//! (github.com/lobehub/lobe-icons, `@lobehub/icons-static-svg` 1.104.0, MIT; its licence
//! is `assets/models/LICENSE`), without their size and title attributes. They draw in
//! the text colour around them.

/// Where the icons come from and their licence, for the licences Settings shows.
pub(super) const ATTRIBUTION: &str = concat!(
    "LOBEHUB ICONS (model and provider marks, monochrome, size attributes removed)\n",
    "Source: https://github.com/lobehub/lobe-icons (@lobehub/icons-static-svg 1.104.0)\n\n",
    include_str!("../../assets/models/LICENSE"),
);

macro_rules! model_icons {
    ($($file:literal),* $(,)?) => {
        /// Every icon file by its path in the application's assets.
        const FILES: &[(&str, &[u8])] = &[
            $(
                (
                    concat!("models/", $file, ".svg"),
                    include_bytes!(concat!("../../assets/models/", $file, ".svg")).as_slice(),
                ),
            )*
        ];
    };
}

model_icons!(
    "anthropic",
    "aws",
    "bedrock",
    "claude",
    "cohere",
    "deepseek",
    "gemini",
    "gemma",
    "google",
    "grok",
    "kimi",
    "meta",
    "minimax",
    "mistral",
    "nvidia",
    "openai",
    "openrouter",
    "qwen",
    "xai",
    "zhipu",
);

/// The icon file at `path`, when it is one of these.
pub(super) fn file(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(known, _)| *known == path)
        .map(|(_, bytes)| *bytes)
}

/// The mark of the family a model belongs to, by words in its name or value
/// (`openrouter/anthropic/claude-opus-4`, `Claude Opus 4`): Claude, GPT, Gemini and the
/// rest, whoever serves it.
pub(super) fn for_model(value: &str, name: &str) -> Option<&'static str> {
    let text = format!("{} {}", name, value).to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| text.contains(word));
    let icon = if has(&["claude"]) {
        "claude"
    } else if has(&["gemma"]) {
        "gemma"
    } else if has(&["gemini"]) {
        "gemini"
    } else if has(&["grok", "x-ai/"]) {
        "grok"
    } else if has(&["deepseek"]) {
        "deepseek"
    } else if has(&["qwen", "qwq"]) {
        "qwen"
    } else if has(&["llama", "meta-llama", "meta/"]) {
        "meta"
    } else if has(&[
        "mistral",
        "ministral",
        "codestral",
        "devstral",
        "magistral",
        "pixtral",
    ]) {
        "mistral"
    } else if has(&["kimi", "moonshot"]) {
        "kimi"
    } else if has(&["glm", "zhipu", "z-ai/"]) {
        "zhipu"
    } else if has(&["nemotron", "nvidia"]) {
        "nvidia"
    } else if has(&["minimax"]) {
        "minimax"
    } else if has(&["cohere", "command-"]) {
        "cohere"
    } else if has(&["gpt", "openai", "codex", "/o1", "/o3", "/o4"]) {
        "openai"
    } else if has(&["amazon.", "amazon/", "nova"]) {
        "aws"
    } else {
        return None;
    };
    path(icon)
}

/// The mark of the provider a model is served by (`openrouter`, `amazon-bedrock`).
pub(super) fn for_provider(provider: &str) -> Option<&'static str> {
    let icon = match provider {
        "anthropic" => "anthropic",
        "openai" | "openai-codex" => "openai",
        "openrouter" => "openrouter",
        "amazon-bedrock" | "bedrock-mantle" => "bedrock",
        "google" | "google-vertex" | "google-gemini-cli" => "google",
        "xai" => "xai",
        "deepseek" => "deepseek",
        "mistral" => "mistral",
        _ => return None,
    };
    path(icon)
}

/// The mark of an agent's own models, which name no provider: its model mark (Claude
/// Code's are Claude, Codex's OpenAI); `None` for an agent with a brand mark of its own.
pub(super) fn for_harness(harness: compi_protocol::chat::ChatHarness) -> Option<&'static str> {
    match harness.entry().mark {
        compi_protocol::chat::AgentMark::Model { file, .. } => path(file),
        compi_protocol::chat::AgentMark::Brand(_) => None,
    }
}

fn path(icon: &str) -> Option<&'static str> {
    FILES.iter().map(|(path, _)| *path).find(|path| {
        path.strip_prefix("models/")
            .and_then(|file| file.strip_suffix(".svg"))
            == Some(icon)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_take_their_familys_mark_whoever_serves_them() {
        assert_eq!(
            for_model("amazon-bedrock/anthropic.claude-3-opus", "Claude Opus 3"),
            Some("models/claude.svg")
        );
        assert_eq!(
            for_model("openrouter/openai/gpt-5", "GPT-5"),
            Some("models/openai.svg")
        );
        assert_eq!(
            for_model("openrouter/google/gemma-3-27b", "Gemma 3 27B"),
            Some("models/gemma.svg")
        );
        assert_eq!(for_model("openrouter/acme/zeta", "Zeta"), None);
        assert_eq!(for_provider("bedrock-mantle"), Some("models/bedrock.svg"));
        assert!(FILES.iter().all(|(path, _)| file(path).is_some()));
        // An agent drawn with a model mark has it here; one with a brand of its own, none.
        use compi_protocol::chat::ChatHarness;
        assert_eq!(for_harness(ChatHarness::Omp), None);
        assert_eq!(for_harness(ChatHarness::Claude), Some("models/claude.svg"));
        assert_eq!(for_harness(ChatHarness::Codex), Some("models/openai.svg"));
    }
}
