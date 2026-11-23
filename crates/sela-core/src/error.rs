#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Message(String),
}

impl DbError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
