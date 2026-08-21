(component
  (import "event-payload-len" (func $event-payload-len (param "index" u64) (result s64)))
  (import "event-payload-byte" (func $event-payload-byte
    (param "index" u64) (param "offset" u64) (result s32)))
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (core func $event-payload-len-core (canon lower (func $event-payload-len)))
  (core func $event-payload-byte-core (canon lower (func $event-payload-byte)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core module $module
    (import "host" "event-payload-len" (func $event-payload-len (param i64) (result i64)))
    (import "host" "event-payload-byte" (func $event-payload-byte (param i64 i64) (result i32)))
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (global $index (mut i64) (i64.const 0))
    (func $record (param $key i64) (param $value i64) (result i32)
      local.get $key
      local.get $value
      call $set-state
      i32.eqz
      if (result i32)
        i32.const 0
      else
        i32.const -4
      end)
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $index
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $length i64)
      (local $byte i32)
      global.get $index
      call $event-payload-len
      local.tee $length
      i64.const 0
      i64.lt_s
      if
        local.get $length
        i32.wrap_i64
        return
      end
      i64.const 820
      local.get $length
      call $record
      i32.eqz
      i32.eqz
      if
        i32.const -4
        return
      end
      local.get $length
      i64.eqz
      if
        i32.const -3
        return
      end
      global.get $index
      i64.const 0
      call $event-payload-byte
      local.tee $byte
      i32.const 0
      i32.lt_s
      if
        local.get $byte
        return
      end
      i64.const 821
      local.get $byte
      i64.extend_i32_u
      call $record
      i32.eqz
      i32.eqz
      if
        i32.const -4
        return
      end
      global.get $index
      local.get $length
      call $event-payload-byte
      local.tee $byte
      i32.const -4
      i32.ne
      if
        i32.const -4
        return
      end
      i64.const 822
      i64.const 4
      call $record
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const -4
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "event-payload-len" (func $event-payload-len-core))
    (export "event-payload-byte" (func $event-payload-byte-core))
    (export "set-state" (func $set-state-core)))
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
