//! The models each agent listed last, in any chat. Settings → Projects offers them for a
//! project's new threads, which have no running agent to ask. Kept in Compi's data
//! folder (`listed-models.json`), so they outlast a restart; a missing or unreadable
//! file is an empty list.

use compi_protocol::chat::{ChatChoice, ChatHarness};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "listed-models.json";
/// Larger files are not read: no agent lists that many models.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// What each agent listed last, by its program (`omp`, `claude`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedModels {
    #[serde(default)]
    agents: BTreeMap<String, AgentModels>,
}

/// One agent's model list as its chat last reported it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentModels {
    #[serde(default)]
    pub choices: Vec<ChatChoice>,
    /// The providers it had a subscription plan for (`anthropic`), which its model
    /// picker lists first.
    #[serde(default)]
    pub plans: Vec<String>,
}

impl ListedModels {
    /// What `harness` listed last; `None` when no chat of it listed any.
    pub fn get(&self, harness: ChatHarness) -> Option<&AgentModels> {
        self.agents.get(harness.program())
    }

    /// Keeps `models` as what `harness` listed; whether that changed anything. An empty
    /// list keeps the last one: an agent still starting lists none.
    pub fn set(&mut self, harness: ChatHarness, models: AgentModels) -> bool {
        if models.choices.is_empty() || self.get(harness) == Some(&models) {
            return false;
        }
        self.agents.insert(harness.program().to_owned(), models);
        true
    }

    /// The lists in Compi's data folder.
    pub fn load() -> Self {
        file_path().map_or_else(|_| Self::default(), |path| Self::read(&path))
    }

    /// Writes the lists into Compi's data folder.
    pub fn save(&self) -> Result<(), String> {
        self.write(&file_path()?)
    }

    fn read(path: &Path) -> Self {
        let small = std::fs::metadata(path).is_ok_and(|metadata| metadata.len() <= MAX_FILE_BYTES);
        small
            .then(|| std::fs::read(path).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Written whole before it takes its name, so a write cut short never replaces it.
    fn write(&self, path: &Path) -> Result<(), String> {
        let failed = |error: &dyn std::fmt::Display| {
            format!(
                "Could not save the agents' model lists to {}: {error}",
                path.display()
            )
        };
        let json = serde_json::to_vec(self).map_err(|error| failed(&error))?;
        let partial = path.with_extension("json.partial");
        std::fs::write(&partial, json).map_err(|error| failed(&error))?;
        std::fs::rename(&partial, path).map_err(|error| failed(&error))
    }
}

fn file_path() -> Result<PathBuf, String> {
    compi_protocol::paths::data_dir()
        .map(|folder| folder.join(FILE_NAME))
        .map_err(|error| format!("Cannot locate Compi's data folder: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(value: &str) -> ChatChoice {
        ChatChoice {
            value: value.into(),
            name: value.into(),
            description: None,
            group: None,
        }
    }

    #[test]
    fn lists_round_trip_and_an_empty_one_keeps_the_last() {
        let mut listed = ListedModels::default();
        let models = AgentModels {
            choices: vec![choice("anthropic/claude-opus-5-5"), choice("openai/gpt-6")],
            plans: vec!["anthropic".into()],
        };
        assert!(listed.set(ChatHarness::Omp, models.clone()));
        assert!(!listed.set(ChatHarness::Omp, models.clone()));
        assert!(!listed.set(ChatHarness::Omp, AgentModels::default()));
        assert_eq!(listed.get(ChatHarness::Omp), Some(&models));
        assert_eq!(listed.get(ChatHarness::Claude), None);

        let folder =
            std::env::temp_dir().join(format!("compi-listed-models-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(FILE_NAME);
        listed.write(&path).unwrap();
        assert_eq!(ListedModels::read(&path), listed);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(ListedModels::read(&path), ListedModels::default());
        assert_eq!(
            ListedModels::read(&folder.join("missing.json")),
            ListedModels::default()
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
