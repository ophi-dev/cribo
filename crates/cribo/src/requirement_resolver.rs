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

use crate::{config::RequirementsConfig, resolver::AUTO_DETECTED_VIRTUALENV_NAMES};

const METADATA_QUERY: &str = include_str!("requirement_resolver.py");

#[derive(Debug, Serialize)]
struct MetadataRequest {
    imports: Vec<MetadataImportRequest>,
    metadata_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MetadataImportRequest {
    name: String,
    preferred_path: Option<String>,
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
    /// Create a resolver backed by the configured interpreter and metadata search paths.
    pub(crate) const fn new(config: &'a RequirementsConfig, metadata_paths: Vec<PathBuf>) -> Self {
        Self {
            config,
            metadata_paths,
        }
    }

    /// Resolve imported module names to normalized PEP 508 requirements.
    pub(crate) fn resolve(
        &self,
        imports: &IndexMap<String, Option<PathBuf>>,
    ) -> Result<IndexSet<String>> {
        let mut requirements = IndexSet::new();
        let mut pending = Vec::new();

        for (import_name, preferred_path) in imports {
            if let Some(requirement) = self.override_for(import_name)? {
                requirements.insert(requirement);
            } else {
                pending.push((import_name.clone(), preferred_path.clone()));
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

        for import_name in imports.keys() {
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

    /// Return the longest matching explicit module-map override.
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

    /// Return whether an import is equal to or nested below a configured prefix.
    fn matches_prefix(prefix: &str, import_name: &str) -> bool {
        import_name == prefix
            || import_name
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('.'))
    }

    /// Query distribution ownership through the bundled Python metadata helper.
    fn query_metadata(
        &self,
        python: &Path,
        imports: Vec<(String, Option<PathBuf>)>,
    ) -> Result<MetadataResponse> {
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
        let imports = imports
            .into_iter()
            .map(|(name, preferred_path)| {
                let preferred_path = preferred_path
                    .map(|path| {
                        path.to_str().map(ToOwned::to_owned).ok_or_else(|| {
                            anyhow!(
                                "Preferred distribution metadata path is not valid UTF-8: {}",
                                path.display()
                            )
                        })
                    })
                    .transpose()?;
                Ok(MetadataImportRequest {
                    name,
                    preferred_path,
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

    /// Select the strongest unambiguous distribution candidate for an import.
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

    /// Convert an import root into a fallback package name when it is PEP 508-compatible.
    fn fallback_requirement(import_name: &str) -> Option<String> {
        let root_import = import_name.split('.').next().unwrap_or(import_name);
        PackageName::new(root_import.to_owned())
            .ok()
            .map(|package_name| package_name.to_string())
    }

    /// Select the Python interpreter used to inspect distribution metadata.
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

        if let Some(candidate) = self.auto_detected_python() {
            return Ok(candidate);
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

    /// Find a conventional virtualenv interpreter above the primary metadata path.
    fn auto_detected_python(&self) -> Option<PathBuf> {
        let first_path = self.metadata_paths.first()?;
        for ancestor in first_path.ancestors() {
            for environment_name in AUTO_DETECTED_VIRTUALENV_NAMES {
                let candidate = Self::environment_python(&ancestor.join(environment_name));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    /// Return the platform-specific Python executable path inside an environment.
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
    use std::fs;

    use tempfile::TempDir;

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

    #[test]
    fn every_virtualenv_name_is_auto_detected() -> Result<()> {
        for environment_name in AUTO_DETECTED_VIRTUALENV_NAMES {
            let temp_dir = TempDir::new()?;
            let project_dir = temp_dir.path().join("project");
            let metadata_path = project_dir.join("src");
            fs::create_dir_all(&metadata_path)?;

            let environment = project_dir.join(environment_name);
            let expected_python = RequirementResolver::environment_python(&environment);
            let python_dir = expected_python
                .parent()
                .context("virtualenv Python path should have a parent")?;
            fs::create_dir_all(python_dir)?;
            fs::write(&expected_python, b"")?;

            let config = RequirementsConfig::default();
            let resolver = RequirementResolver::new(&config, vec![metadata_path]);
            assert_eq!(resolver.auto_detected_python(), Some(expected_python));
        }
        Ok(())
    }

    #[test]
    fn python_helper_handles_absolute_paths_and_broken_metadata() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let helper_path = temp_dir.path().join("requirement_resolver.py");
        fs::write(&helper_path, METADATA_QUERY)?;

        let config = RequirementsConfig::default();
        let resolver = RequirementResolver::new(&config, Vec::new());
        let python = resolver.python_executable()?;
        let test_script = r#"
import runpy
import sys

namespace = runpy.run_path(sys.argv[1], run_name="requirement_resolver_test")
build_distribution_index = namespace["build_distribution_index"]
file_score = namespace["file_score"]
distribution_candidates = namespace["distribution_candidates"]
search_path_candidates = namespace["search_path_candidates"]

assert file_score(
    "shared.beta",
    "/editable/src/shared/beta/__init__.py",
    ["/editable/src"],
) == (4002, "installed file")
assert file_score(
    "polars",
    "/site-packages/pandera/api/polars/__init__.py",
    ["/site-packages"],
) is None

class BrokenMetadata:
    def get(self, key):
        return "Broken-Distribution" if key == "Name" else None

    def get_all(self, key):
        return ()

class BrokenDistribution:
    metadata = BrokenMetadata()

    @property
    def files(self):
        raise RuntimeError("malformed files metadata")

    def read_text(self, filename):
        raise RuntimeError("malformed top-level metadata")

broken_index = build_distribution_index([BrokenDistribution()])
assert distribution_candidates("broken", broken_index) == []

class UnreadableDistribution:
    @property
    def metadata(self):
        raise RuntimeError("malformed distribution metadata")

assert build_distribution_index([UnreadableDistribution()]) == {
    "prefix": {},
    "exact": {},
}

class CountingMetadata:
    def get(self, key):
        return "Counting-Distribution" if key == "Name" else None

    def get_all(self, key):
        return ["counted"] if key == "Import-Name" else ()

class CountingDistribution:
    def __init__(self):
        self.metadata_reads = 0
        self.files_reads = 0
        self.top_level_reads = 0

    @property
    def metadata(self):
        self.metadata_reads += 1
        return CountingMetadata()

    @property
    def files(self):
        self.files_reads += 1
        return ()

    def read_text(self, filename):
        self.top_level_reads += 1
        return ""

counting_distribution = CountingDistribution()
counting_index = build_distribution_index([counting_distribution])
assert distribution_candidates("counted", counting_index)
assert distribution_candidates("counted.child", counting_index)
assert counting_distribution.metadata_reads == 1
assert counting_distribution.files_reads == 1
assert counting_distribution.top_level_reads == 1

empty_index = {"prefix": {}, "exact": {}}
assert search_path_candidates(
    "counted",
    [
        ("/editable/src", empty_index),
        ("/site-packages", counting_index),
    ],
    "/editable/src",
)[0]["distribution"] == "Counting-Distribution"
"#;
        let output = Command::new(python)
            .args(["-I", "-c", test_script])
            .arg(helper_path)
            .output()
            .context("failed to run requirement resolver Python helper test")?;

        assert!(
            output.status.success(),
            "Python helper test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    #[test]
    fn metadata_query_rejects_conflicting_import_declarations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let metadata_dir = temp_dir.path().join("conflicting-1.0.dist-info");
        fs::create_dir_all(&metadata_dir)?;
        fs::write(
            metadata_dir.join("METADATA"),
            "Metadata-Version: 2.5\n\
             Name: conflicting\n\
             Version: 1.0\n\
             Import-Name: shared\n\
             Import-Namespace: shared\n",
        )?;

        let config = RequirementsConfig::default();
        let resolver = RequirementResolver::new(&config, vec![temp_dir.path().to_path_buf()]);
        let python = resolver.python_executable()?;
        let error = resolver
            .query_metadata(&python, vec![("shared".to_owned(), None)])
            .expect_err("conflicting Core Metadata declarations should fail");

        assert!(
            error
                .to_string()
                .contains("declares shared in both Import-Name and Import-Namespace")
        );
        Ok(())
    }

    #[test]
    fn metadata_query_ignores_unrelated_conflicting_import_declarations() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let conflicting_metadata = temp_dir.path().join("conflicting-1.0.dist-info");
        fs::create_dir_all(&conflicting_metadata)?;
        fs::write(
            conflicting_metadata.join("METADATA"),
            "Metadata-Version: 2.5\n\
             Name: conflicting\n\
             Version: 1.0\n\
             Import-Name: unrelated\n\
             Import-Namespace: unrelated\n",
        )?;

        let requested_metadata = temp_dir.path().join("requested_provider-1.0.dist-info");
        fs::create_dir_all(&requested_metadata)?;
        fs::write(
            requested_metadata.join("METADATA"),
            "Metadata-Version: 2.5\n\
             Name: requested-provider\n\
             Version: 1.0\n\
             Import-Name: requested\n",
        )?;

        let config = RequirementsConfig::default();
        let resolver = RequirementResolver::new(&config, vec![temp_dir.path().to_path_buf()]);
        let python = resolver.python_executable()?;
        let response = resolver.query_metadata(&python, vec![("requested".to_owned(), None)])?;
        let candidates = response
            .resolutions
            .get("requested")
            .context("metadata query should include the requested import")?;

        assert_eq!(
            RequirementResolver::select_candidate("requested", candidates)?,
            Some("requested-provider".to_owned())
        );
        Ok(())
    }

    #[test]
    fn metadata_query_uses_utf8_protocol() -> Result<()> {
        let config = RequirementsConfig::default();
        let resolver = RequirementResolver::new(&config, Vec::new());
        let python = resolver.python_executable()?;
        let import_name = "m\u{00f3}dulo".to_owned();

        let response = resolver.query_metadata(&python, vec![(import_name.clone(), None)])?;

        assert!(response.resolutions.contains_key(&import_name));
        Ok(())
    }
}
