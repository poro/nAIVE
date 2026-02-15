use std::collections::{HashMap, VecDeque};
use std::path::Path;

use serde::{Deserialize, Serialize};

/// A game event with a type name and arbitrary payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameEvent {
    pub event_type: String,
    pub data: HashMap<String, serde_json::Value>,
    pub timestamp: f64,
}

/// Event schema loaded from events/schema.yaml for validation.
#[derive(Debug, Clone, Deserialize)]
pub struct EventSchema {
    #[serde(default)]
    pub events: HashMap<String, EventFieldSchema>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventFieldSchema {
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// Central event bus with ring buffer logging.
pub struct EventBus {
    /// Listeners keyed by event type. Each listener gets an ID.
    listeners: HashMap<String, Vec<(u64, Box<dyn Fn(&GameEvent) + Send + Sync>)>>,
    next_listener_id: u64,
    /// Ring buffer log of recent events.
    log: VecDeque<GameEvent>,
    log_capacity: usize,
    /// Optional event schema for validation.
    schema: Option<EventSchema>,
    /// File logger path (if enabled).
    log_file: Option<std::path::PathBuf>,
    /// Total time for timestamps.
    total_time: f64,
    /// Pending events to be flushed.
    pending: Vec<GameEvent>,
}

impl EventBus {
    pub fn new(log_capacity: usize) -> Self {
        Self {
            listeners: HashMap::new(),
            next_listener_id: 0,
            log: VecDeque::with_capacity(log_capacity),
            log_capacity,
            schema: None,
            log_file: None,
            total_time: 0.0,
            pending: Vec::new(),
        }
    }

    /// Load event schema from YAML file.
    pub fn load_schema(&mut self, project_root: &Path) {
        let schema_path = project_root.join("events/schema.yaml");
        if schema_path.exists() {
            match std::fs::read_to_string(&schema_path) {
                Ok(contents) => match serde_yaml::from_str::<EventSchema>(&contents) {
                    Ok(schema) => {
                        tracing::info!(
                            "Loaded event schema: {} event types",
                            schema.events.len()
                        );
                        self.schema = Some(schema);
                    }
                    Err(e) => tracing::warn!("Failed to parse event schema: {}", e),
                },
                Err(e) => tracing::warn!("Failed to read event schema: {}", e),
            }
        }
    }

    /// Enable file logging.
    pub fn enable_file_logging(&mut self, path: std::path::PathBuf) {
        self.log_file = Some(path);
    }

    /// Emit an event. Queues it for processing during flush.
    pub fn emit(&mut self, event_type: &str, data: HashMap<String, serde_json::Value>) {
        // Validate against schema if available
        if let Some(schema) = &self.schema {
            if let Some(event_schema) = schema.events.get(event_type) {
                for required_field in &event_schema.fields {
                    if !data.contains_key(required_field) {
                        tracing::warn!(
                            "Event '{}' missing required field '{}' per schema",
                            event_type,
                            required_field
                        );
                    }
                }
            }
        }

        let event = GameEvent {
            event_type: event_type.to_string(),
            data,
            timestamp: self.total_time,
        };

        self.pending.push(event);
    }

    /// Emit a simple event with no data.
    pub fn emit_simple(&mut self, event_type: &str) {
        self.emit(event_type, HashMap::new());
    }

    /// Register a listener for an event type. Returns a listener ID for removal.
    pub fn listen<F>(&mut self, event_type: &str, callback: F) -> u64
    where
        F: Fn(&GameEvent) + Send + Sync + 'static,
    {
        let id = self.next_listener_id;
        self.next_listener_id += 1;
        self.listeners
            .entry(event_type.to_string())
            .or_default()
            .push((id, Box::new(callback)));
        id
    }

    /// Remove a listener by ID.
    pub fn remove_listener(&mut self, listener_id: u64) {
        for listeners in self.listeners.values_mut() {
            listeners.retain(|(id, _)| *id != listener_id);
        }
    }

    /// Flush pending events: notify Rust listeners, log to ring buffer and file.
    /// Returns the flushed events for Lua listener dispatch.
    pub fn flush(&mut self) -> Vec<GameEvent> {
        let events: Vec<GameEvent> = self.pending.drain(..).collect();

        for event in &events {
            // Notify Rust listeners
            if let Some(listeners) = self.listeners.get(&event.event_type) {
                for (_id, callback) in listeners {
                    callback(event);
                }
            }

            // Add to ring buffer
            if self.log.len() >= self.log_capacity {
                self.log.pop_front();
            }
            self.log.push_back(event.clone());

            // File logging
            if let Some(log_path) = &self.log_file {
                if let Ok(json) = serde_json::to_string(event) {
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(log_path)
                        .and_then(|mut f| {
                            use std::io::Write;
                            writeln!(f, "{}", json)
                        });
                }
            }
        }

        events
    }

    /// Advance time.
    pub fn tick(&mut self, dt: f64) {
        self.total_time += dt;
    }

    /// Get the event log (ring buffer).
    pub fn get_log(&self) -> &VecDeque<GameEvent> {
        &self.log
    }

    /// Get total elapsed time.
    pub fn total_time(&self) -> f64 {
        self.total_time
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_event_bus_emit_and_flush() {
        let mut bus = EventBus::new(100);
        let received = Arc::new(Mutex::new(Vec::new()));

        let recv_clone = received.clone();
        bus.listen("test.event", move |event| {
            recv_clone.lock().unwrap().push(event.clone());
        });

        let mut data = HashMap::new();
        data.insert("value".to_string(), serde_json::json!(42));
        bus.emit("test.event", data);
        bus.flush();

        let events = received.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "test.event");
        assert_eq!(events[0].data["value"], 42);
    }

    #[test]
    fn test_ring_buffer_capacity() {
        let mut bus = EventBus::new(3);
        for i in 0..5 {
            let mut data = HashMap::new();
            data.insert("i".to_string(), serde_json::json!(i));
            bus.emit("tick", data);
        }
        bus.flush();

        assert_eq!(bus.get_log().len(), 3);
        // Should have events 2, 3, 4 (oldest dropped)
        assert_eq!(bus.get_log()[0].data["i"], 2);
    }

    #[test]
    fn test_remove_listener() {
        let mut bus = EventBus::new(100);
        let received = Arc::new(Mutex::new(0));

        let recv_clone = received.clone();
        let id = bus.listen("test", move |_| {
            *recv_clone.lock().unwrap() += 1;
        });

        bus.emit_simple("test");
        bus.flush();
        assert_eq!(*received.lock().unwrap(), 1);

        bus.remove_listener(id);
        bus.emit_simple("test");
        bus.flush();
        assert_eq!(*received.lock().unwrap(), 1); // Still 1, listener was removed
    }
}
