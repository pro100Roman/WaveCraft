use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

fn normalize_recording_path(mut path: PathBuf) -> PathBuf {
    let has_dsv_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("dsv"))
        .unwrap_or(false);
    if !has_dsv_extension {
        path.set_extension("dsv");
    }
    path
}

fn available_recording_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("capture");
    for index in 2_u32.. {
        let candidate = parent.join(format!("{stem}-{index}.dsv"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("the recording file suffix cannot be exhausted")
}

pub(crate) fn choose_recording_save_path(current_path: &str) -> Option<PathBuf> {
    let default_name = Path::new(current_path.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("capture.dsv");
    rfd::FileDialog::new()
        .set_title("Save complete WaveCraft capture")
        .add_filter("WaveCraft capture", &["dsv"])
        .set_file_name(default_name)
        .save_file()
        .map(normalize_recording_path)
        .map(available_recording_path)
}

pub(crate) fn choose_recording_to_open() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Open capture")
        .add_filter("Supported captures", &["dsv", "sal"])
        .add_filter("WaveCraft capture", &["dsv"])
        .add_filter("Saleae Logic 2 capture", &["sal"])
        .pick_file()
}

fn normalize_saleae_path(mut path: PathBuf) -> PathBuf {
    let has_sal_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("sal"));
    if !has_sal_extension {
        path.set_extension("sal");
    }
    path
}

pub(crate) fn choose_saleae_export_path() -> Option<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Export Saleae Logic 2 capture")
        .add_filter("Saleae Logic 2 capture", &["sal"])
        .set_file_name("capture.sal")
        .save_file()
        .map(normalize_saleae_path)
}

pub(crate) fn create_saleae_export_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("failed to create or replace {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_paths_use_the_dsv_extension() {
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture")),
            PathBuf::from("capture.dsv")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.DSV")),
            PathBuf::from("capture.DSV")
        );
        assert_eq!(
            normalize_recording_path(PathBuf::from("capture.txt")),
            PathBuf::from("capture.dsv")
        );
    }

    #[test]
    fn saleae_paths_use_the_sal_extension() {
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture")),
            PathBuf::from("capture.sal")
        );
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture.SAL")),
            PathBuf::from("capture.SAL")
        );
        assert_eq!(
            normalize_saleae_path(PathBuf::from("capture.zip")),
            PathBuf::from("capture.sal")
        );
    }

    #[test]
    fn saleae_export_replaces_the_selected_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "wavecraft-saleae-export-{}-{unique}.sal",
            std::process::id()
        ));
        std::fs::write(&path, b"previous capture").unwrap();

        let file = create_saleae_export_file(&path).unwrap();
        assert_eq!(file.metadata().unwrap().len(), 0);
        drop(file);

        std::fs::remove_file(path).unwrap();
    }
}
