//! Wasmtime + WASI embedding spike (story 03.01).
//! Measures compile_ms and invoke_ms for a minimal add module.

use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let wat = r#"
        (module
          (func (export "add") (param i32 i32) (result i32)
            local.get 0
            local.get 1
            i32.add)
        )
    "#;

    let compile_start = Instant::now();
    let engine = wasmtime::Engine::default();
    let wasm_bytes = wat::parse_str(wat)?;
    let module = wasmtime::Module::new(&engine, &wasm_bytes)?;
    let compile_ms = compile_start.elapsed().as_millis();

    let invoke_start = Instant::now();
    let mut store = wasmtime::Store::new(&engine, ());
    let instance = wasmtime::Instance::new(&mut store, &module, &[])?;
    let add = instance.get_typed_func::<(i32, i32), i32>(&mut store, "add")?;
    let sum = add.call(&mut store, (2, 40))?;
    let invoke_ms = invoke_start.elapsed().as_millis();

    assert_eq!(sum, 42);
    println!("spike_wasm compile_ms={compile_ms} invoke_ms={invoke_ms} result={sum}");
    Ok(())
}
