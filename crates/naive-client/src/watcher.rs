use std::path::{Path, PathBuf};
use std::sync::mpsc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Events sent from the watcher thread to the main loop.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum WatchEvent {
    ShaderChanged(PathBuf),
    SceneChanged(PathBuf),
    MaterialChanged(PathBuf),
    PipelineChanged(PathBuf),
    SplatChanged(PathBuf),
    ScriptChanged(PathBuf),
}

/// Saving another scene document must not replace the currently running scene.
pub fn is_active_scene(scene: Option<&Path>, changed: &Path) -> bool {
    let Some(scene) = scene else {
        return false;
    };
    scene == changed
        || match (scene.canonicalize(), changed.canonicalize()) {
            (Ok(scene), Ok(changed)) => scene == changed,
            _ => false,
        }
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
                                        let _ = tx.send(WatchEvent::MaterialChanged(path.clone()));
                                    } else if path_str.contains("pipelines") {
                                        tracing::info!("Pipeline file changed: {:?}", path);
                                        let _ = tx.send(WatchEvent::PipelineChanged(path.clone()));
                                    }
                                }
                                "ply" => {
                                    tracing::info!("Splat file changed: {:?}", path);
                                    let _ = tx.send(WatchEvent::SplatChanged(path.clone()));
                                }
                                "lua" => {
                                    tracing::info!("Script file changed: {:?}", path);
                                    let _ = tx.send(WatchEvent::ScriptChanged(path.clone()));
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

    // Watch shaders, scenes, materials, and pipelines directories
    let dirs = [
        project_root.join("shaders"),
        project_root.join("scenes"),
        project_root.join("assets/materials"),
        project_root.join("assets/splats"),
        project_root.join("pipelines"),
        project_root.join("logic"),
    ];

    for dir in &dirs {
        if dir.exists() {
            watcher.watch(dir, RecursiveMode::Recursive)?;
            tracing::info!("File watcher started on: {:?}", dir);
        }
    }

    Ok((watcher, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_another_scene_does_not_reload_the_active_scene() {
        let root = std::env::temp_dir().join(format!("naive-watch-scene-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let active = root.join("main.yaml");
        let saved = root.join("saved.yaml");
        std::fs::write(&active, "name: Main").unwrap();
        std::fs::write(&saved, "name: Saved").unwrap();
        assert!(is_active_scene(Some(&active), &active));
        assert!(is_active_scene(
            Some(&active),
            &active.canonicalize().unwrap()
        ));
        assert!(!is_active_scene(Some(&active), &saved));
        assert!(!is_active_scene(None, &saved));
        std::fs::remove_dir_all(root).unwrap();
    }
}
