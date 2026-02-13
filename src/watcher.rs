use std::path::{Path, PathBuf};
use std::sync::mpsc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Events sent from the watcher thread to the main loop.
#[derive(Debug)]
pub enum WatchEvent {
    ShaderChanged(PathBuf),
    SceneChanged(PathBuf),
    MaterialChanged(PathBuf),
}

/// Creates a file watcher on the project directory and returns a receiver
/// for change events. The watcher must be kept alive.
pub fn start_watching_all(
    project_root: &Path,
) -> Result<(RecommendedWatcher, mpsc::Receiver<WatchEvent>), notify::Error> {
    let (tx, rx) = mpsc::channel();

    let mut watcher =
        notify::recommended_watcher(move |result: Result<Event, notify::Error>| match result {
            Ok(event) => match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => {
                    for path in &event.paths {
                        if let Some(ext) = path.extension() {
                            let ext_str = ext.to_string_lossy();
                            let path_str = path.to_string_lossy();

                            match ext_str.as_ref() {
                                "slang" | "wgsl" => {
                                    tracing::info!("Shader file changed: {:?}", path);
                                    let _ = tx.send(WatchEvent::ShaderChanged(path.clone()));
                                }
                                "yaml" | "yml" => {
                                    if path_str.contains("scenes") {
                                        tracing::info!("Scene file changed: {:?}", path);
                                        let _ = tx.send(WatchEvent::SceneChanged(path.clone()));
                                    } else if path_str.contains("materials") {
                                        tracing::info!("Material file changed: {:?}", path);
                                        let _ =
                                            tx.send(WatchEvent::MaterialChanged(path.clone()));
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            },
            Err(e) => {
                tracing::error!("File watcher error: {:?}", e);
            }
        })?;

    // Watch shaders, scenes, and materials directories
    let dirs = [
        project_root.join("shaders"),
        project_root.join("scenes"),
        project_root.join("assets/materials"),
    ];

    for dir in &dirs {
        if dir.exists() {
            watcher.watch(dir, RecursiveMode::Recursive)?;
            tracing::info!("File watcher started on: {:?}", dir);
        }
    }

    Ok((watcher, rx))
}
