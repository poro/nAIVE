use std::path::{Path, PathBuf};
use std::sync::mpsc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Events sent from the watcher thread to the main loop.
#[derive(Debug)]
pub enum WatchEvent {
    ShaderChanged(PathBuf),
}

/// Creates a file watcher on the given directory and returns a receiver
/// for shader change events. The watcher must be kept alive.
pub fn start_watching(
    watch_dir: &Path,
) -> Result<(RecommendedWatcher, mpsc::Receiver<WatchEvent>), notify::Error> {
    let (tx, rx) = mpsc::channel();

    let mut watcher =
        notify::recommended_watcher(move |result: Result<Event, notify::Error>| match result {
            Ok(event) => match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => {
                    for path in &event.paths {
                        if let Some(ext) = path.extension() {
                            if ext == "slang" || ext == "wgsl" {
                                tracing::info!("Shader file changed: {:?}", path);
                                let _ = tx.send(WatchEvent::ShaderChanged(path.clone()));
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

    watcher.watch(watch_dir, RecursiveMode::Recursive)?;
    tracing::info!("File watcher started on: {:?}", watch_dir);

    Ok((watcher, rx))
}
