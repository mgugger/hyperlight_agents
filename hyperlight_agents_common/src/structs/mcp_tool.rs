use alloc::collections::BTreeMap;
use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Tool {
    /**Optional additional tool information.
    Display name precedence order is: title, annotations.title, then name.*/
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    /**A human-readable description of the tool.
    This can be used by clients to improve the LLM's understanding of available tools. It can be thought of like a "hint" to the model.*/
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
    ///See [specification/2025-06-18/basic/index#general-fields] for notes on _meta usage.
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<::serde_json::Map<::alloc::string::String, ::serde_json::Value>>,
    ///Intended for programmatic or logical use, but used as a display name in past specs or fallback (if title isn't present).
    pub name: ::alloc::string::String,
    #[serde(
        rename = "outputSchema",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_schema: Option<ToolOutputSchema>,
    /**Intended for UI and end-user contexts — optimized to be human-readable and easily understood,
    even by those unfamiliar with domain-specific terminology.
    If not provided, the name should be used for display (except for Tool,
    where annotations.title should be given precedence over using name,
    if present).*/
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct ToolInputSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, ::serde_json::Map<String, ::serde_json::Value>>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(rename = "type")]
    type_: String,
}
impl ToolInputSchema {
    pub fn new(
        required: Vec<String>,
        properties: Option<BTreeMap<String, ::serde_json::Map<String, ::serde_json::Value>>>,
    ) -> Self {
        Self {
            properties,
            required,
            type_: "object".to_string(),
        }
    }
    pub fn type_(&self) -> &String {
        &self.type_
    }
    pub fn type_name() -> String {
        "object".to_string()
    }
}

#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, Default)]
pub struct ToolAnnotations {
    /**If true, the tool may perform destructive updates to its environment.
    If false, the tool performs only additive updates.
    (This property is meaningful only when readOnlyHint == false)
    Default: true*/
    #[serde(
        rename = "destructiveHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
    /**If true, calling the tool repeatedly with the same arguments
    will have no additional effect on the its environment.
    (This property is meaningful only when readOnlyHint == false)
    Default: false*/
    #[serde(
        rename = "idempotentHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent_hint: Option<bool>,
    /**If true, this tool may interact with an "open world" of external
    entities. If false, the tool's domain of interaction is closed.
    For example, the world of a web search tool is open, whereas that
    of a memory tool is not.
    Default: true*/
    #[serde(
        rename = "openWorldHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world_hint: Option<bool>,
    /**If true, the tool does not modify its environment.
    Default: false*/
    #[serde(
        rename = "readOnlyHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    ///A human-readable title for the tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug)]
pub struct ToolOutputSchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, ::serde_json::Map<String, ::serde_json::Value>>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required: Vec<String>,
    #[serde(rename = "type")]
    type_: String,
}
impl ToolOutputSchema {
    pub fn new(
        required: Vec<String>,
        properties: Option<BTreeMap<String, ::serde_json::Map<String, ::serde_json::Value>>>,
    ) -> Self {
        Self {
            properties,
            required,
            type_: "object".to_string(),
        }
    }
    pub fn type_(&self) -> &String {
        &self.type_
    }
    pub fn type_name() -> String {
        "object".to_string()
    }
}

#[derive(::serde::Deserialize, ::serde::Serialize, Clone, Debug, Default)]
pub struct Annotations {
    /**Describes who the intended customer of this object or data is.
    It can include multiple entries to indicate content useful for multiple audiences (e.g., ["user", "assistant"]).*/
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audience: Vec<Role>,
    /**The moment the resource was last modified, as an ISO 8601 formatted string.
    Should be an ISO 8601 formatted string (e.g., "2025-01-12T15:00:58Z").
    Examples: last activity timestamp in an open file, timestamp when the resource
    was attached, etc.*/
    #[serde(
        rename = "lastModified",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
}

#[derive(
    ::serde::Deserialize,
    ::serde::Serialize,
    Clone,
    Copy,
    Debug,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
)]
pub enum Role {
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "user")]
    User,
}
impl alloc::fmt::Display for Role {
    fn fmt(&self, f: &mut ::alloc::fmt::Formatter<'_>) -> ::alloc::fmt::Result {
        match *self {
            Self::Assistant => write!(f, "assistant"),
            Self::User => write!(f, "user"),
        }
    }
}
