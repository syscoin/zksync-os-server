use std::path::{Path, PathBuf};

/// Supported configuration file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Yaml,
}

// SYSCOIN: Keep extension detection fallible so callers with Result-returning APIs
// can report unsupported config paths without unwinding the process.
#[derive(Debug, thiserror::Error)]
pub enum ConfigFormatError {
    #[error(
        "Unsupported config file extension for path '{}'. Supported extensions are .json, .yaml and .yml",
        path.display()
    )]
    UnsupportedExtension { path: PathBuf },
}

impl ConfigFormat {
    /// Detects the configuration format from a file path based on its extension.
    pub fn from_path(path: &Path) -> Result<Self, ConfigFormatError> {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_lowercase());

        match extension.as_deref() {
            Some("yaml") | Some("yml") => Ok(ConfigFormat::Yaml),
            Some("json") => Ok(ConfigFormat::Json),
            _ => Err(ConfigFormatError::UnsupportedExtension {
                path: path.to_path_buf(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_extension() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.json")).unwrap(),
            ConfigFormat::Json
        );
    }

    #[test]
    fn test_yaml_extension() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.yaml")).unwrap(),
            ConfigFormat::Yaml
        );
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.yml")).unwrap(),
            ConfigFormat::Yaml
        );
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.JSON")).unwrap(),
            ConfigFormat::Json
        );
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.YAML")).unwrap(),
            ConfigFormat::Yaml
        );
        assert_eq!(
            ConfigFormat::from_path(Path::new("config.YML")).unwrap(),
            ConfigFormat::Yaml
        );
    }

    #[test]
    fn test_unsupported_extension() {
        assert!(ConfigFormat::from_path(Path::new("config.toml")).is_err());
    }

    #[test]
    fn test_missing_extension() {
        assert!(ConfigFormat::from_path(Path::new("config")).is_err());
    }
}
