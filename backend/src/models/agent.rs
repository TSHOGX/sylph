use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSpec {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub flags: HashMap<String, FlagSpec>,
    pub commands: HashMap<String, CommandSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagSpec {
    pub description: Option<String>,
    pub choices: Vec<String>,
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub description: Option<String>,
    pub args: HashMap<String, ArgSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgSpec {
    #[serde(rename = "type")]
    pub arg_type: String,
    pub description: Option<String>,
    pub required: Option<bool>,
}
