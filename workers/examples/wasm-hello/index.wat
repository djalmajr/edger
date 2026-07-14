(module
  (memory (export "memory") 1)
  (data (i32.const 512) "[[\"content-type\",\"text/plain\"],[\"x-wasm-abi\",\"v2\"]]")
  (data (i32.const 600) "wasm path: ")
  (global $heap (mut i32) (i32.const 1024))

  (func (export "edger_alloc") (param $len i32) (result i32)
    (local $ptr i32)
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr
  )

  (func $copy (param $dst i32) (param $src i32) (param $len i32)
    (local $i i32)
    loop $copy_loop
      local.get $i
      local.get $len
      i32.lt_u
      if
        local.get $dst
        local.get $i
        i32.add
        local.get $src
        local.get $i
        i32.add
        i32.load8_u
        i32.store8
        local.get $i
        i32.const 1
        i32.add
        local.set $i
        br $copy_loop
      end
    end
  )

  (func (export "edger_handle") (param $req_ptr i32) (param $req_len i32) (result i64)
    (local $method_len i32)
    (local $uri_len i32)
    (local $uri_ptr i32)
    (local $body_ptr i32)
    (local $body_len i32)
    (local $frame_len i32)

    local.get $req_ptr
    i32.load
    local.set $method_len
    local.get $req_ptr
    i32.const 4
    i32.add
    i32.load
    local.set $uri_len
    local.get $req_ptr
    i32.const 16
    i32.add
    local.get $method_len
    i32.add
    local.set $uri_ptr

    i32.const 4159
    local.set $body_ptr
    i32.const 11
    local.get $uri_len
    i32.add
    local.set $body_len
    i32.const 74
    local.get $uri_len
    i32.add
    local.set $frame_len

    i32.const 4096
    i32.const 200
    i32.store16
    i32.const 4100
    i32.const 51
    i32.store
    i32.const 4104
    local.get $body_len
    i32.store
    i32.const 4108
    i32.const 512
    i32.const 51
    call $copy
    local.get $body_ptr
    i32.const 600
    i32.const 11
    call $copy
    local.get $body_ptr
    i32.const 11
    i32.add
    local.get $uri_ptr
    local.get $uri_len
    call $copy

    i32.const 4096
    i64.extend_i32_u
    local.get $frame_len
    i64.extend_i32_u
    i64.const 32
    i64.shl
    i64.or
  )
)
