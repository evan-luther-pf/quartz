(component
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (import "resolve" (func $resolve (param "slot" u64) (result s64)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core func $resolve-core (canon lower (func $resolve)))
  (core module $module
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (import "host" "resolve" (func $resolve (param i64) (result i64)))
    (global $delay (mut i64) (i64.const 0))
    (global $phase (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $delay
      i64.const 0
      global.set $phase
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      (local $value i64)
      global.get $phase
      global.get $delay
      i64.lt_u
      if
        global.get $phase
        i64.const 100
        i64.add
        global.get $phase
        call $set-state
        local.tee $status
        i32.eqz
        if
          global.get $phase
          i64.const 1
          i64.add
          global.set $phase
          i32.const 0
          return
        end
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      i64.const 1
      call $resolve
      local.tee $value
      i64.const 0
      i64.lt_s
      if
        local.get $value
        i32.wrap_i64
        return
      end
      i64.const 900
      local.get $value
      call $set-state
      local.tee $status
      i32.eqz
      if
        i32.const 1
        return
      end
      i32.const 0
      local.get $status
      i32.sub)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "set-state" (func $set-state-core))
    (export "resolve" (func $resolve-core)))
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
