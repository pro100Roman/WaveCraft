use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

const FORMAT_VERSION: u32 = 1;
const SETTINGS_FILE_NAME: &str = "capture-settings.json";

/// A portable, versioned representation of capture settings.
///
/// The same document can back the application defaults today and named,
/// user-selected profile files later. Keeping instrument values as stable
/// string keys avoids coupling persisted files to private GUI types.
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub(crate) struct CaptureSettingsDocument {
    pub(crate) format_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) profiles: CaptureSettingsProfiles,
}

impl Default for CaptureSettingsDocument {
    fn default() -> Self {
        Self {
            format_version: FORMAT_VERSION,
            name: None,
            profiles: CaptureSettingsProfiles::default(),
        }
    }
}

impl CaptureSettingsDocument {
    fn validate(self) -> Result<Self, String> {
        if self.format_version != FORMAT_VERSION {
            return Err(format!(
                "unsupported capture-settings format version {} (expected {FORMAT_VERSION})",
                self.format_version
            ));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub(crate) struct CaptureSettingsProfiles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) logic_analyzer: Option<CaptureSettingsProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) oscilloscope: Option<CaptureSettingsProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) radio_frequency: Option<CaptureSettingsProfile>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub(crate) struct CaptureSettingsProfile {
    #[serde(default)]
    pub(crate) values: BTreeMap<String, String>,
}

impl CaptureSettingsProfile {
    pub(crate) fn new(values: BTreeMap<String, String>) -> Self {
        Self { values }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CaptureSettingsRepository {
    path: PathBuf,
}

impl CaptureSettingsRepository {
    pub(crate) fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub(crate) fn for_current_user() -> Result<Self, String> {
        Ok(Self::at(default_settings_path()?))
    }

    pub(crate) fn load(&self) -> Result<CaptureSettingsDocument, String> {
        let source = match fs::read_to_string(&self.path) {
            Ok(source) => source,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(CaptureSettingsDocument::default())
            }
            Err(error) => {
                return Err(format!(
                    "failed to read capture settings {}: {error}",
                    self.path.display()
                ))
            }
        };
        serde_json::from_str::<CaptureSettingsDocument>(&source)
            .map_err(|error| {
                format!(
                    "failed to parse capture settings {}: {error}",
                    self.path.display()
                )
            })?
            .validate()
    }

    pub(crate) fn save(&self, document: &CaptureSettingsDocument) -> Result<(), String> {
        document.clone().validate()?;
        let directory = self
            .path
            .parent()
            .ok_or_else(|| "capture-settings path has no parent directory".to_owned())?;
        fs::create_dir_all(directory).map_err(|error| {
            format!(
                "failed to create capture-settings directory {}: {error}",
                directory.display()
            )
        })?;

        let encoded = serde_json::to_vec_pretty(document)
            .map_err(|error| format!("failed to encode capture settings: {error}"))?;
        let temporary_path = temporary_path(&self.path);
        let result = (|| {
            let mut file = File::create(&temporary_path).map_err(|error| {
                format!(
                    "failed to create temporary capture-settings file {}: {error}",
                    temporary_path.display()
                )
            })?;
            file.write_all(&encoded).map_err(|error| {
                format!(
                    "failed to write temporary capture-settings file {}: {error}",
                    temporary_path.display()
                )
            })?;
            file.sync_all().map_err(|error| {
                format!(
                    "failed to flush temporary capture-settings file {}: {error}",
                    temporary_path.display()
                )
            })?;
            drop(file);
            replace_file(&temporary_path, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| SETTINGS_FILE_NAME.into());
    file_name.push(".tmp");
    path.with_file_name(file_name)
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), String> {
    match fs::rename(source, destination) {
        Ok(()) => Ok(()),
        #[cfg(target_os = "windows")]
        Err(error)
            if destination.exists()
                && matches!(
                    error.kind(),
                    ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
                ) =>
        {
            fs::remove_file(destination).map_err(|remove_error| {
                format!(
                    "failed to replace capture settings {}: {remove_error}",
                    destination.display()
                )
            })?;
            fs::rename(source, destination).map_err(|rename_error| {
                format!(
                    "failed to install capture settings {}: {rename_error}",
                    destination.display()
                )
            })
        }
        Err(error) => Err(format!(
            "failed to install capture settings {}: {error}",
            destination.display()
        )),
    }
}

fn default_settings_path() -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("WAVECRAFT_CONFIG_DIR") {
        return Ok(PathBuf::from(directory).join(SETTINGS_FILE_NAME));
    }

    #[cfg(target_os = "windows")]
    let directory = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("WaveCraft"));

    #[cfg(target_os = "macos")]
    let directory = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|path| path.join("Library/Application Support/WaveCraft"));

    #[cfg(all(unix, not(target_os = "macos")))]
    let directory = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|path| path.join("wavecraft"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|path| path.join(".config/wavecraft"))
        });

    #[cfg(not(any(unix, target_os = "windows")))]
    let directory = None;

    directory
        .map(|directory| directory.join(SETTINGS_FILE_NAME))
        .ok_or_else(|| "could not determine the current user's configuration directory".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_repository(test_name: &str) -> CaptureSettingsRepository {
        let directory = std::env::temp_dir().join(format!(
            "wavecraft-capture-settings-{test_name}-{}",
            std::process::id()
        ));
        CaptureSettingsRepository::at(directory.join(SETTINGS_FILE_NAME))
    }

    #[test]
    fn missing_file_loads_default_profiles() {
        let repository = test_repository("missing");
        let _ = fs::remove_file(repository.path());

        assert_eq!(
            repository.load().expect("missing settings should load"),
            CaptureSettingsDocument::default()
        );
    }

    #[test]
    fn saving_again_replaces_each_profile_in_one_document() {
        let repository = test_repository("replace");
        let _ = fs::remove_file(repository.path());
        let mut document = CaptureSettingsDocument::default();
        document.profiles.logic_analyzer = Some(CaptureSettingsProfile::new(BTreeMap::from([(
            "logic.sample_rate_hz".to_owned(),
            "1000000".to_owned(),
        )])));
        document.profiles.oscilloscope = Some(CaptureSettingsProfile::new(BTreeMap::from([(
            "scope.sample_rate_hz".to_owned(),
            "5000000".to_owned(),
        )])));
        repository.save(&document).expect("first save should work");

        document.profiles.logic_analyzer = Some(CaptureSettingsProfile::new(BTreeMap::from([(
            "logic.sample_rate_hz".to_owned(),
            "10000000".to_owned(),
        )])));
        repository.save(&document).expect("replacement should work");

        let loaded = repository.load().expect("saved settings should load");
        assert_eq!(loaded, document);
        assert_eq!(
            loaded
                .profiles
                .oscilloscope
                .expect("scope profile should remain")
                .values["scope.sample_rate_hz"],
            "5000000"
        );
        let _ = fs::remove_file(repository.path());
        if let Some(parent) = repository.path().parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    #[test]
    fn rejects_unknown_format_versions() {
        let repository = test_repository("version");
        if let Some(parent) = repository.path().parent() {
            fs::create_dir_all(parent).expect("test directory should be created");
        }
        fs::write(repository.path(), r#"{"format_version":2,"profiles":{}}"#)
            .expect("test settings should be written");

        assert!(repository
            .load()
            .expect_err("unknown versions should fail")
            .contains("unsupported capture-settings format version"));
        let _ = fs::remove_file(repository.path());
        if let Some(parent) = repository.path().parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}
