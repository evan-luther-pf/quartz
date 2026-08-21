(component
  (import "set-state" (func $set-state (param "key" u64) (param "value" u64) (result s32)))
  (import "call-provider" (func $call-provider (param "slot" u64) (param "operation" u64)
    (param "arg0" u64) (param "arg1" u64) (result s64)))
  (import "apply-patch" (func $apply-patch (param "index" u64)
    (param "base-revision" u64) (result s32)))
  (core func $set-state-core (canon lower (func $set-state)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $apply-patch-core (canon lower (func $apply-patch)))
  (core module $module
    (import "host" "set-state" (func $set-state (param i64 i64) (result i32)))
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "apply-patch" (func $apply-patch (param i64 i64) (result i32)))
    (global $config (mut i64) (i64.const 0))
    (func (export "start") (param $value i64) (result i64)
      local.get $value
      global.set $config
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $decision i64)
      (local $status i32)
      i64.const 2
      i64.const 1
      global.get $config
      i64.const 4294967295
      i64.and
      global.get $config
      i64.const 32
      i64.shr_u
      i64.const 2147483647
      i64.and
      call $call-provider
      local.tee $decision
      i64.const 0
      i64.lt_s
      if
        local.get $decision
        i32.wrap_i64
        return
      end
      local.get $decision
      i64.const 1
      i64.eq
      if
        global.get $config
        i64.const 4294967295
        i64.and
        global.get $config
        i64.const 32
        i64.shr_u
        i64.const 2147483647
        i64.and
        call $apply-patch
        local.set $status
      else
        i32.const 7
        local.set $status
      end
      i64.const 700
      local.get $status
      i64.extend_i32_u
      call $set-state
      local.tee $status
      i32.eqz
      if
      else
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      global.get $config
      i64.const 0
      i64.lt_s
      if (result i32)
        i32.const -7
      else
        i32.const 1
      end)
    (func (export "invoke") (param i64 i64 i64 i64) (result i64) i64.const -4)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "set-state" (func $set-state-core))
    (export "call-provider" (func $call-provider-core))
    (export "apply-patch" (func $apply-patch-core)))
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
