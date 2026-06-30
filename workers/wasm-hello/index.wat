(module
  (memory (export "memory") 1)
  (data (i32.const 0) "wasm-hello")
  (func (export "http_status") (result i32) i32.const 200)
  (func (export "http_body_len") (result i32) i32.const 10)
)
