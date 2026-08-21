(component
  (import "resume-snapshot" (func $resume-snapshot
    (param "event-index" u64) (param "snapshot-index" u64) (param "value" u64)
    (result s32)))
  (core func $resume-snapshot-core (canon lower (func $resume-snapshot)))
  (core module $module
    (import "host" "resume-snapshot" (func $resume-snapshot (param i64 i64 i64) (result i32)))
    (global $event-value (mut i64) (i64.const 360569445166350336))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      i64.eqz
      if
      else
        local.get $config
        global.set $event-value
      end
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      i64.const 0
      i64.const 0
      global.get $event-value
      call $resume-snapshot
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const -4
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "resume-snapshot" (func $resume-snapshot-core)))
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
