use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct NetworkInfo {
    pub network_name: String,
    pub network_prefix: String,
}
