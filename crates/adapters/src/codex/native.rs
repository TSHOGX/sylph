//! Native catalogue and acquisition inventory; semantic mapping is server-owned.

use anyhow::Result;
use sylph_protocol::{Fidelity, NativeId, NativeSource, NativeSummary, StoreFilter};

pub const NATIVE_FIDELITY: Fidelity = Fidelity::Blocks;
pub fn enumerate(filter: &StoreFilter) -> Result<Vec<NativeSummary>> {
    crate::native::inventory::enumerate("codex", filter)
}
pub fn lookup(id: &NativeId) -> Result<Option<NativeSummary>> {
    crate::native::inventory::lookup("codex", id)
}
pub fn sources(id: &NativeId) -> Result<Vec<NativeSource>> {
    crate::native::inventory::sources("codex", id)
}
