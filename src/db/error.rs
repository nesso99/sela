#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

impl DbError {
    pub fn message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}
