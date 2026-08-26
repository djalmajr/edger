//! Shim: o vocabulário MCP desceu para `edger_core::mcp` (o orchestrator
//! precisa dos descritores para o endpoint HTTP e não pode depender deste
//! crate — a dependência é inversa). Re-export mantém os imports existentes.

pub use edger_core::mcp::{
    capability_contract, capability_descriptors, http_tool_descriptors, tool_descriptors,
    CapabilityDescriptor, ToolDescriptor, EDGER_SCHEMA_VERSION, MCP_PROTOCOL_VERSION,
};
