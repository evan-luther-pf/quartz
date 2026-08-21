(component
  (import "append-event" (func $append-event (param "index" u64) (param "value" u64) (result s32)))
  (core func $append-event-core (canon lower (func $append-event)))
  (core module $module
    (import "host" "append-event" (func $append-event (param i64 i64) (result i32)))
    (global $config (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $config
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      i64.const 0
      global.get $config
      call $append-event
      local.set $status
      i32.const 0
      local.get $status
      i32.sub)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host (export "append-event" (func $append-event-core)))
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
