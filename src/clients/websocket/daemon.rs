
pub struct ChiaMessageFilter {
    pub destination: Option<String>,
    pub command: Option<String>,
    pub request_id: Option<String>,
    pub origin: Option<String>,
}
impl ChiaMessageFilter {
    pub fn matches(&self, msg: Arc<ChiaMessage>) -> bool {
        if let Some(s) = &self.destination {
            if *s != msg.destination {
                return false;
            }
        }
        if let Some(s) = &self.command {
            if *s != msg.command {
                return false;
            }
        }
        if let Some(s) = &self.request_id {
            if *s != msg.request_id {
                return false;
            }
        }
        if let Some(s) = &self.origin {
            if *s != msg.origin {
                return false;
            }
        }
        true
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RegisterMessage {
    service: String,
}

pub struct ChiaMessageHandler {
    filter: ChiaMessageFilter,
    handle: Arc<dyn MessageHandler + Send + Sync>,
}
impl ChiaMessageHandler {
    pub fn new(filter: ChiaMessageFilter, handle: Arc<dyn MessageHandler + Send + Sync>) -> Self {
        ChiaMessageHandler { filter, handle }
    }
}

#[derive(Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChiaMessage {
    pub destination: String,
    pub command: String,
    pub request_id: String,
    pub origin: String,
    pub ack: bool,
    pub data: Value,
}