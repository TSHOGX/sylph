use sylph_protocol::{Facet, LinkFacets, SessionCapabilities, UnavailableReason};

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkState {
    pub link: LinkFacets,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    pub supports: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<SessionCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authorization: Option<String>,
    #[serde(rename = "controlBlock", skip_serializing_if = "Option::is_none")]
    pub control_block: Option<ControlBlock>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlBlock {
    pub kind: String,
    pub reason: String,
}

impl LinkState {
    pub fn failed(reason: UnavailableReason) -> Self {
        Self {
            link: LinkFacets {
                observe: Facet::available(),
                control: Facet::unavailable(reason),
            },
            agent: None,
            supports: Vec::new(),
            capabilities: None,
            authorization: None,
            control_block: None,
        }
    }

    pub fn detached_remote() -> Self {
        Self::failed(UnavailableReason::EnvironmentUnreachable)
    }

    pub fn from_remote_capabilities_with_link(
        agent: String,
        capabilities: SessionCapabilities,
        supports: Vec<String>,
        link: Option<LinkFacets>,
    ) -> Self {
        Self {
            link: link.unwrap_or(LinkFacets {
                observe: Facet::available(),
                control: Facet::available(),
            }),
            agent: Some(agent),
            supports,
            capabilities: Some(capabilities),
            authorization: None,
            control_block: None,
        }
    }
}
