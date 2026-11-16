//! Lua events API registration.

use super::{EventSubscriptions, ScriptRuntime, SharedEventBus};
use mlua::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

impl ScriptRuntime {
    /// Register event bus API (events.emit, events.on, events.off).
    pub fn register_event_api(
        &self,
        event_bus: SharedEventBus,
        lua_listeners: Rc<RefCell<HashMap<String, Vec<mlua::RegistryKey>>>>,
        next_listener_id: Rc<RefCell<u64>>,
        listener_id_map: Rc<RefCell<HashMap<u64, (String, usize)>>>,
    ) -> Result<(), String> {
        let globals = self.lua.globals();
        let events_table = self.lua.create_table().map_err(|e| e.to_string())?;
        *self.subscriptions.borrow_mut() = Some(EventSubscriptions {
            listeners: lua_listeners.clone(),
            ids: listener_id_map.clone(),
        });

        // events.emit(event_type, data_table)
        let bus = event_bus.clone();
        let emit_fn = self
            .lua
            .create_function(move |_, (event_type, data): (String, LuaTable)| {
                let mut bus = bus.borrow_mut();
                let mut map = HashMap::new();
                for (key, val) in data.pairs::<String, LuaValue>().flatten() {
                    let json_val = match val {
                        LuaValue::Integer(i) => {
                            serde_json::Value::Number(serde_json::Number::from(i))
                        }
                        LuaValue::Number(n) => serde_json::Number::from_f64(n)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null),
                        LuaValue::String(s) => {
                            serde_json::Value::String(s.to_string_lossy().to_string())
                        }
                        LuaValue::Boolean(b) => serde_json::Value::Bool(b),
                        _ => serde_json::Value::Null,
                    };
                    map.insert(key, json_val);
                }
                bus.emit(&event_type, map);
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        events_table
            .set("emit", emit_fn)
            .map_err(|e| e.to_string())?;

        // events.on(event_type, callback) -> listener_id
        let listeners = lua_listeners.clone();
        let next_id = next_listener_id.clone();
        let id_map = listener_id_map.clone();
        let owners = self.listener_owners.clone();
        let environments = self.listener_environments.clone();
        let active_entity = self.active_entity.clone();
        let on_fn = self
            .lua
            .create_function(move |lua, (event_type, callback): (String, LuaFunction)| {
                let mut listeners = listeners.borrow_mut();
                let mut next_id = next_id.borrow_mut();
                let mut id_map = id_map.borrow_mut();

                let owner = active_entity.get();
                let environment = callback.environment();
                let active = active_entity.clone();
                let wrapped = lua.create_function(move |_, args: LuaMultiValue| {
                    struct Restore<'a>(
                        &'a std::cell::Cell<Option<hecs::Entity>>,
                        Option<hecs::Entity>,
                    );
                    impl Drop for Restore<'_> {
                        fn drop(&mut self) {
                            self.0.set(self.1);
                        }
                    }
                    let _restore = Restore(&active, active.replace(owner));
                    callback.call::<()>(args)
                })?;
                let owned_key = lua
                    .create_registry_value(wrapped)
                    .map_err(mlua::Error::external)?;
                let list = listeners.entry(event_type.clone()).or_default();
                let index = list.len();
                list.push(owned_key);

                let listener_id = *next_id;
                *next_id += 1;
                id_map.insert(listener_id, (event_type, index));
                if let Some(owner) = owner {
                    owners.borrow_mut().insert(listener_id, owner);
                    if let Some(environment) = environment {
                        environments.borrow_mut().insert(listener_id, environment);
                    }
                }

                Ok(listener_id)
            })
            .map_err(|e| e.to_string())?;
        events_table.set("on", on_fn).map_err(|e| e.to_string())?;

        // events.off(listener_id) - remove a listener
        let listeners = lua_listeners.clone();
        let id_map = listener_id_map.clone();
        let owners = self.listener_owners.clone();
        let environments = self.listener_environments.clone();
        let staged_removals = self.staged_listener_removals.clone();
        let off_fn = self
            .lua
            .create_function(move |_, listener_id: u64| {
                if let Some(removals) = staged_removals.borrow_mut().as_mut() {
                    removals.push(listener_id);
                    return Ok(());
                }
                owners.borrow_mut().remove(&listener_id);
                environments.borrow_mut().remove(&listener_id);
                let mut listeners = listeners.borrow_mut();
                let mut id_map = id_map.borrow_mut();

                if let Some((event_type, index)) = id_map.remove(&listener_id) {
                    if let Some(list) = listeners.get_mut(&event_type) {
                        if index < list.len() {
                            list.remove(index);
                            // Reindex the id_map entries for this event type
                            for (et, idx) in id_map.values_mut() {
                                if *et == event_type && *idx > index {
                                    *idx -= 1;
                                }
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        events_table.set("off", off_fn).map_err(|e| e.to_string())?;

        globals
            .set("events", events_table)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
