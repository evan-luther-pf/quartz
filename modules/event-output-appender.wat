(component
  (import "event-output-set-len" (func $set-len (param "index" u64) (param "length" u64) (result s32)))
  (import "event-output-write-byte" (func $write-byte (param "index" u64) (param "offset" u64) (param "value" u32) (result s32)))
  (import "resume-event-output" (func $resume-output (param "event-index" u64) (param "output-index" u64) (param "value" u64) (result s32)))
  (core func $set-len-core (canon lower (func $set-len)))
  (core func $write-byte-core (canon lower (func $write-byte)))
  (core func $resume-output-core (canon lower (func $resume-output)))
  (core module $module
    (import "host" "set-len" (func $set-len (param i64 i64) (result i32)))
    (import "host" "write-byte" (func $write-byte (param i64 i64 i32) (result i32)))
    (import "host" "resume-output" (func $resume-output (param i64 i64 i64) (result i32)))
    (func (export "start") (param i64) (result i64) i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      i64.const 0
      i64.const 5
      call $set-len
      local.tee $status
      if
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      i64.const 0 i64.const 0 i32.const 103 call $write-byte drop
      i64.const 0 i64.const 1 i32.const 117 call $write-byte drop
      i64.const 0 i64.const 2 i32.const 101 call $write-byte drop
      i64.const 0 i64.const 3 i32.const 115 call $write-byte drop
      i64.const 0 i64.const 4 i32.const 116 call $write-byte drop
      i64.const 0
      i64.const 0
      i64.const 7
      call $resume-output
      i32.eqz)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "set-len" (func $set-len-core))
    (export "write-byte" (func $write-byte-core))
    (export "resume-output" (func $resume-output-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "invoke") (param "instance" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)
    (canon lift (core func $instance "invoke")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
