use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::folder::FolderHistoryEntry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderGroup {
    pub id: i32,
    pub name: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderGroupDetail {
    pub id: i32,
    pub name: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub folders: Vec<FolderHistoryEntry>,
}
