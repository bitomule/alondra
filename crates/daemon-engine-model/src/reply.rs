//! What a route answers: the status and the JSON body both the local listener and the relay
//! carry. The server turns it into an HTTP response.
use http::StatusCode;
use serde_json::Value;

pub struct Reply {
    pub status: StatusCode,
    pub body: Value,
}
