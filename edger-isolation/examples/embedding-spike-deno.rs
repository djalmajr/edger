//! Deno_core embedding spike placeholder (story 03.01).
//!
//! Full V8/deno_core integration requires pinned deno_core + platform singleton.
//! Pendency: enable `deno` feature when toolchain validated — see spike.md.
//!
//! This example validates edger-core wire types can simulate a request payload
//! without booting V8 (compile-time spike gate).

use edger_core::SerializedRequest;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let start = Instant::now();
    let req = SerializedRequest {
        method: "GET".into(),
        uri: "/hello".into(),
        headers: vec![("accept".into(), "text/plain".into())],
        body: None,
        request_id: "spike-deno-1".into(),
        base_href: None,
    };
    let json = serde_json::to_string(&req)?;
    let back: SerializedRequest = serde_json::from_str(&json)?;
    let exec_ms = start.elapsed().as_millis();
    assert_eq!(back.uri, "/hello");
    println!(
        "spike_deno_wire_sim exec_ms={exec_ms} note=deno_core_boot_pending_V8_toolchain"
    );
    Ok(())
}