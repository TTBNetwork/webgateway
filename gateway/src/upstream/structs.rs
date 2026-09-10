use std::sync::Arc;

use shared::objectid::ObjectId;

#[derive(Debug, Clone)]
pub struct ConnectionRequest {
    pub host: Arc<String>,
    pub path: Arc<String>,
    pub req_id: ObjectId,
}
