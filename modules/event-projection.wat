(component
  (import "event-count" (func $event-count (result s64)))
  (import "read-event" (func $read-event (param "index" u64) (result s64)))
  (import "publish" (func $publish (param "slot" u64) (param "value" u64) (result s32)))
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (core func $event-count-core (canon lower (func $event-count)))
  (core func $read-event-core (canon lower (func $read-event)))
  (core func $publish-core (canon lower (func $publish)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core module $module
    (import "host" "event-count" (func $event-count (result i64)))
    (import "host" "read-event" (func $read-event (param i64) (result i64)))
    (import "host" "publish" (func $publish (param i64 i64) (result i32)))
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (global $initialized (mut i32) (i32.const 0))
    (global $index (mut i64) (i64.const 0))
    (global $count (mut i64) (i64.const 0))
    (global $sum (mut i64) (i64.const 0))
    (func (export "start") (param i64) (result i64)
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $value i64)
      (local $status i32)
      global.get $initialized
      i32.eqz
      if
        call $event-count
        local.tee $value
        i64.const 0
        i64.lt_s
        if
          local.get $value
          i32.wrap_i64
          return
        end
        local.get $value
        global.set $count
        i32.const 1
        global.set $initialized
        i32.const 0
        return
      end
      global.get $index
      global.get $count
      i64.lt_u
      if
        global.get $index
        call $read-event
        local.tee $value
        i64.const 0
        i64.lt_s
        if
          local.get $value
          i32.wrap_i64
          return
        end
        global.get $sum
        local.get $value
        i64.add
        global.set $sum
        global.get $index
        i64.const 1
        i64.add
        global.set $index
        i32.const 0
        return
      end
      i64.const 901
      global.get $sum
      call $set-state
      drop
      i64.const 5
      global.get $sum
      call $publish
      local.set $status
      local.get $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "event-count" (func $event-count-core))
    (export "read-event" (func $read-event-core))
    (export "publish" (func $publish-core))
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
