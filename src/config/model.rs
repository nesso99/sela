use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::db::Engine;

/// A connection Birch can reopen. The password is not part of this value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnection {
    pub id: String,
    pub name: String,
    pub engine: Engine,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub database: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub ssl: bool,
}

impl SavedConnection {
    pub fn new(engine: Engine) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: String::new(),
            port: engine.default_port(),
            engine,
            host: "127.0.0.1".into(),
            username: String::new(),
            database: String::new(),
            file_path: String::new(),
            ssl: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the connection a name.".into());
        }
        match self.engine {
            Engine::Sqlite => {
                if self.file_path.trim().is_empty() {
                    return Err("Choose a SQLite file path.".into());
                }
            }
            Engine::Postgres | Engine::Mysql => {
                if self.host.trim().is_empty() {
                    return Err("Host is required.".into());
                }
                if self.port == 0 {
                    return Err("Port must be between 1 and 65535.".into());
                }
                if self.username.trim().is_empty() {
                    return Err("User is required.".into());
                }
                if self.database.trim().is_empty() {
                    return Err("Database name is required.".into());
                }
            }
        }
        Ok(())
    }

    pub fn endpoint_label(&self) -> String {
        match self.engine {
            Engine::Sqlite => self.file_path.clone(),
            Engine::Postgres | Engine::Mysql => {
                format!(
                    "{}@{}:{}/{}",
                    self.username, self.host, self.port, self.database
                )
            }
        }
    }
}
