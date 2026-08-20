(component
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core module $module
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (global $phase (mut i32) (i32.const 0))
    (func (export "start") (param i64) (result i64)
      i32.const 0
      global.set $phase
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      global.get $phase
      i32.eqz
      if
        i64.const 10
        i64.const 99
        call $set-state
        local.tee $status
        i32.eqz
        if
          i32.const 1
          global.set $phase
          i32.const 0
          return
        end
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      i32.const -7)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
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
