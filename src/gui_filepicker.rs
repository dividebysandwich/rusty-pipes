use anyhow::Result;
use rfd::FileDialog;
use std::path::PathBuf;

/// Shows a native file picker dialog.
pub fn pick_file(title: &str, filters: &[(&str, &[&str])]) -> Result<Option<PathBuf>> {
    log::info!("Opening file picker with title: {}", title);
    
    let mut dialog = FileDialog::new().set_title(title).set_directory("/");
    
    for (name, extensions) in filters {
        dialog = dialog.add_filter(*name, *extensions);
    }

    let file = dialog.pick_file(); // This is a blocking call

    match file {
        Some(path) => {
            log::info!("File selected: {}", path.display());
            Ok(Some(path))
        }
        None => {
            log::info!("File selection cancelled.");
            Ok(None)
        }
    }
}