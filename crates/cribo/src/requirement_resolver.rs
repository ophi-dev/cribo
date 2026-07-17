use std::{
    env,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
};

use anyhow::{Context, Result, anyhow, bail};
use indexmap::{IndexMap, IndexSet};
use log::{debug, warn};
use pep508_rs::{PackageName, Requirement, VerbatimUrl};
use serde::{Deserialize, Serialize};

use crate::config::RequirementsConfig;

const METADATA_QUERY: &str = include_str!("requirement_resolver.py");

#[derive(Debug, Serialize)]
struct MetadataRequest {
    imports: Vec<String>,
    metadata_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MetadataResponse {
    resolutions: IndexMap<String, Vec<DistributionCandidate>>,
}

#[derive(Debug, Deserialize)]
struct DistributionCandidate {
    distribution: String,
    score: u16,
    evidence: String,
}

#[derive(Debug)]
pub(crate) struct RequirementResolver<'a> {
    config: &'a RequirementsConfig,
    metadata_paths: Vec<PathBuf>,
}

impl<'a> RequirementResolver<'a> {
    pub(crate) const fn new(config: &'a RequirementsConfig, metadata_paths: Vec<PathBuf>) -> Self {
        Self {
            config,
            metadata_paths,
        }
    }

    pub(crate) fn resolve(&self, imports: &IndexSet<String>) -> Result<IndexSet<String>> {
        let mut requirements = IndexSet::new();
        let mut pending = Vec::new();

        for import_name in imports {
            if let Some(requirement) = self.override_for(import_name)? {
                requirements.insert(requirement);
            } else {
                pending.push(import_name.clone());
            }
        }

        if pending.is_empty() {
            return Ok(requirements);
        }

        let python = self.python_executable()?;
        debug!(
            "Resolving {} import requirements with {}",
            pending.len(),
            python.display()
        );
        let response = self.query_metadata(&python, pending)?;

        for import_name in imports {
            if self.override_for(import_name)?.is_some() {
                continue;
            }
            let candidates = response.resolutions.get(import_name).ok_or_else(|| {
                anyhow!("Python metadata query omitted import '{import_name}' from its response")
            })?;
            if let Some(requirement) = Self::select_candidate(import_name, candidates)? {
                requirements.insert(requirement);
            } else if let Some(fallback) = Self::fallback_requirement(import_name) {
                warn!(
                    "Could not map import '{import_name}' to installed distribution metadata; \
                     using '{fallback}'"
                );
                requirements.insert(fallback);
            } else {
                warn!(
                    "Could not map import '{import_name}' to installed distribution metadata, and \
                     its root is not a valid requirement name; skipping it"
                );
            }
        }

        Ok(requirements)
    }

    fn override_for(&self, import_name: &str) -> Result<Option<String>> {
        let mapping = self
            .config
            .module_map
            .iter()
            .filter(|(prefix, _)| Self::matches_prefix(prefix, import_name))
            .max_by_key(|(prefix, _)| prefix.split('.').count());

        let Some((prefix, requirement)) = mapping else {
            return Ok(None);
        };
        let parsed = Requirement::<VerbatimUrl>::from_str(requirement).with_context(|| {
            format!(
                "Invalid PEP 508 requirement '{requirement}' configured for import prefix \
                 '{prefix}'"
            )
        })?;
        Ok(Some(parsed.to_string()))
    }

    fn matches_prefix(prefix: &str, import_name: &str) -> bool {
        import_name == prefix
            || import_name
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('.'))
    }

    fn query_metadata(&self, python: &Path, imports: Vec<String>) -> Result<MetadataResponse> {
        let metadata_paths = self
            .metadata_paths
            .iter()
            .map(|path| {
                path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
                    anyhow!(
                        "Distribution metadata path is not valid UTF-8: {}",
                        path.display()
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let request = MetadataRequest {
            imports,
            metadata_paths,
        };
        let request_json =
            serde_json::to_vec(&request).context("Failed to serialize Python metadata request")?;

        let mut child = Command::new(python)
            .args(["-I", "-c", METADATA_QUERY])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to run Python interpreter '{}'; configure --python or \
                     requirements.python",
                    python.display()
                )
            })?;
        child
            .stdin
            .take()
            .context("Python metadata process did not expose stdin")?
            .write_all(&request_json)
            .context("Failed to send request to Python metadata process")?;

        let output = child
            .wait_with_output()
            .context("Failed to wait for Python metadata process")?;
        if !output.status.success() {
            bail!(
                "Python metadata query failed with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        serde_json::from_slice(&output.stdout).with_context(|| {
            format!(
                "Python metadata query returned invalid JSON: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn select_candidate(
        import_name: &str,
        candidates: &[DistributionCandidate],
    ) -> Result<Option<String>> {
        let Some(highest_score) = candidates.iter().map(|candidate| candidate.score).max() else {
            return Ok(None);
        };
        let winners: Vec<&DistributionCandidate> = candidates
            .iter()
            .filter(|candidate| candidate.score == highest_score)
            .collect();
        if winners.len() > 1 {
            let descriptions = winners
                .iter()
                .map(|candidate| format!("{} ({})", candidate.distribution, candidate.evidence))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "Cannot determine a requirement for import '{import_name}': multiple installed \
                 distributions provide equally strong evidence: {descriptions}. Configure \
                 requirements.module-map for this import."
            );
        }

        let winner = winners
            .first()
            .expect("candidate winners should contain the highest-scoring candidate");
        let package_name = PackageName::new(winner.distribution.clone()).with_context(|| {
            format!(
                "Installed distribution '{}' has an invalid package name",
                winner.distribution
            )
        })?;
        Ok(Some(package_name.to_string()))
    }

    fn fallback_requirement(import_name: &str) -> Option<String> {
        let root_import = import_name.split('.').next().unwrap_or(import_name);
        PackageName::new(root_import.to_owned())
            .ok()
            .map(|package_name| package_name.to_string())
    }

    fn python_executable(&self) -> Result<PathBuf> {
        if let Some(python) = &self.config.python {
            return Ok(python.clone());
        }

        for variable in ["VIRTUAL_ENV", "CONDA_PREFIX"] {
            if let Ok(environment) = env::var(variable) {
                let candidate = Self::environment_python(Path::new(&environment));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }

        if let Some(first_path) = self.metadata_paths.first() {
            for ancestor in first_path.ancestors() {
                let candidate = Self::environment_python(&ancestor.join(".venv"));
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }

        for command in ["python3", "python"] {
            if Command::new(command)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
            {
                return Ok(PathBuf::from(command));
            }
        }

        Err(anyhow!(
            "Could not find a Python interpreter for distribution metadata; use --python or set \
             requirements.python"
        ))
    }

    fn environment_python(environment: &Path) -> PathBuf {
        if cfg!(windows) {
            environment.join("Scripts").join("python.exe")
        } else {
            environment.join("bin").join("python")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_override_prefix_wins_and_accepts_pep508() -> Result<()> {
        let config = RequirementsConfig {
            python: None,
            module_map: IndexMap::from([
                ("google".to_owned(), "google-base>=1".to_owned()),
                (
                    "google.cloud.storage".to_owned(),
                    "google-cloud-storage[grpc]>=2".to_owned(),
                ),
            ]),
        };
        let resolver = RequirementResolver::new(&config, Vec::new());

        assert_eq!(
            resolver.override_for("google.cloud.storage.client")?,
            Some("google-cloud-storage[grpc]>=2".to_owned())
        );
        assert_eq!(
            resolver.override_for("google.protobuf")?,
            Some("google-base>=1".to_owned())
        );
        Ok(())
    }

    #[test]
    fn equal_metadata_candidates_are_rejected() {
        let candidates = [
            DistributionCandidate {
                distribution: "first-provider".to_owned(),
                score: 2001,
                evidence: "core metadata Import-Namespace".to_owned(),
            },
            DistributionCandidate {
                distribution: "second-provider".to_owned(),
                score: 2001,
                evidence: "core metadata Import-Namespace".to_owned(),
            },
        ];

        let error = RequirementResolver::select_candidate("shared", &candidates)
            .expect_err("shared namespace providers must be ambiguous");
        assert!(error.to_string().contains("first-provider"));
        assert!(error.to_string().contains("second-provider"));
    }

    #[test]
    fn invalid_import_name_has_no_fallback_requirement() {
        assert_eq!(RequirementResolver::fallback_requirement("_typeshed"), None);
    }
}
