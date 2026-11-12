//! Which model tutors: the service, its address, the key and the model name.

use anyhow::Result;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Anthropic,
    OpenAi,
    HuggingFace,
    /// A llama.cpp server, or any other server with an OpenAI-style API.
    Local,
}

impl Kind {
    pub const ALL: [Kind; 4] = [
        Kind::Anthropic,
        Kind::OpenAi,
        Kind::HuggingFace,
        Kind::Local,
    ];

    fn id(self) -> &'static str {
        match self {
            Kind::Anthropic => "anthropic",
            Kind::OpenAi => "openai",
            Kind::HuggingFace => "huggingface",
            Kind::Local => "local",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Anthropic => "Anthropic",
            Kind::OpenAi => "OpenAI",
            Kind::HuggingFace => "Hugging Face",
            Kind::Local => "Local model",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Kind::Anthropic => "Claude. Needs an API key from console.anthropic.com.",
            Kind::OpenAi => "GPT models. Needs an API key from platform.openai.com.",
            Kind::HuggingFace => "Open models. Needs a token from huggingface.co/settings/tokens.",
            Kind::Local => "A llama.cpp server on this machine, or any OpenAI-style URL.",
        }
    }

    pub fn default_url(self) -> &'static str {
        match self {
            Kind::Anthropic => "https://api.anthropic.com/v1",
            Kind::OpenAi => "https://api.openai.com/v1",
            Kind::HuggingFace => "https://router.huggingface.co/v1",
            Kind::Local => "http://localhost:8080/v1",
        }
    }

    /// Shown in the model picker until the provider's own list arrives,
    /// and if it never does.
    pub fn presets(self) -> &'static [&'static str] {
        match self {
            Kind::Anthropic => &[
                "claude-fable-5-2",
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-5-5",
            ],
            Kind::OpenAi => &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"],
            Kind::HuggingFace => &["openai/gpt-oss-120b", "deepseek-ai/DeepSeek-R1-0528"],
            Kind::Local => &[],
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Kind::Anthropic => "claude-fable-5-2",
            Kind::OpenAi => "gpt-5.6-sol",
            Kind::HuggingFace => "openai/gpt-oss-120b",
            // llama-server answers with whichever model it loaded.
            Kind::Local => "local",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provider {
    pub kind: Kind,
    pub base_url: String,
    pub key: String,
    pub model: String,
}

impl Provider {
    pub fn new(kind: Kind, key: &str) -> Self {
        Self {
            kind,
            base_url: kind.default_url().to_string(),
            key: key.to_string(),
            model: kind.default_model().to_string(),
        }
    }

    /// The first key found in the environment decides the provider.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        [
            ("ANTHROPIC_API_KEY", Kind::Anthropic),
            ("OPENAI_API_KEY", Kind::OpenAi),
            ("HF_TOKEN", Kind::HuggingFace),
        ]
        .into_iter()
        .find_map(|(name, kind)| get(name).map(|key| Self::new(kind, key.trim())))
    }

    pub fn load(path: &Path) -> Option<Self> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
        let kind = Kind::ALL.into_iter().find(|k| v["provider"] == k.id())?;
        Some(Self {
            kind,
            base_url: v["base_url"].as_str()?.to_string(),
            key: v["key"].as_str()?.to_string(),
            model: v["model"].as_str()?.to_string(),
        })
    }

    /// Writes the settings, key included, to a file only the owner can read.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(&json!({
            "provider": self.kind.id(),
            "base_url": self.base_url,
            "key": self.key,
            "model": self.model,
        }))?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)?.write_all(text.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_with_owner_only_permissions() {
        let path = std::env::temp_dir().join(format!("socratik-test-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut provider = Provider::new(Kind::Local, "");
        provider.base_url = "http://localhost:9999/v1".into();
        provider.save(&path).unwrap();
        assert_eq!(Provider::load(&path), Some(provider));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_file(&path).unwrap();
        assert_eq!(Provider::load(&path), None);
    }

    #[test]
    fn picks_the_first_key_in_the_environment() {
        let only_openai =
            |name: &str| (name == "OPENAI_API_KEY").then(|| " sk-test \n".to_string());
        let provider = Provider::from_env(only_openai).unwrap();
        assert_eq!(
            (provider.kind, provider.key.as_str()),
            (Kind::OpenAi, "sk-test")
        );
        assert_eq!(provider.base_url, "https://api.openai.com/v1");
        assert_eq!(Provider::from_env(|_| None), None);
    }
}
