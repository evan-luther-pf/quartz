(component
  (import "call-provider" (func $call-provider
    (param "slot" u64) (param "operation" u64) (param "arg0" u64) (param "arg1" u64)
    (result s64)))
  (import "workspace-len" (func $workspace-len (param "index" u64) (result s64)))
  (import "workspace-set-len" (func $workspace-set-len
    (param "index" u64) (param "length" u64) (result s32)))
  (import "workspace-write-byte" (func $workspace-write-byte
    (param "index" u64) (param "offset" u64) (param "value" u32) (result s32)))
  (import "publish-workspace" (func $publish-workspace (param "index" u64) (result s32)))
  (core func $call-provider-core (canon lower (func $call-provider)))
  (core func $workspace-len-core (canon lower (func $workspace-len)))
  (core func $workspace-set-len-core (canon lower (func $workspace-set-len)))
  (core func $workspace-write-byte-core (canon lower (func $workspace-write-byte)))
  (core func $publish-workspace-core (canon lower (func $publish-workspace)))
  (core module $module
    (import "host" "call-provider" (func $call-provider (param i64 i64 i64 i64) (result i64)))
    (import "host" "workspace-len" (func $workspace-len (param i64) (result i64)))
    (import "host" "workspace-set-len" (func $workspace-set-len (param i64 i64) (result i32)))
    (import "host" "workspace-write-byte" (func $workspace-write-byte (param i64 i64 i32) (result i32)))
    (import "host" "publish-workspace" (func $publish-workspace (param i64) (result i32)))
    (global $byte (mut i32) (i32.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      i32.wrap_i64
      global.set $byte
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $length i64)
      (local $status i32)
      i64.const 0
      call $workspace-len
      local.tee $length
      i64.const 0
      i64.lt_s
      if
        local.get $length
        i32.wrap_i64
        return
      end
      i64.const 0
      local.get $length
      i64.const 1
      i64.add
      call $workspace-set-len
      local.tee $status
      i32.eqz
      i32.eqz
      if
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      i64.const 0
      local.get $length
      global.get $byte
      call $workspace-write-byte
      local.tee $status
      i32.eqz
      i32.eqz
      if
        i32.const 0
        local.get $status
        i32.sub
        return
      end
      i64.const 12
      i64.const 1
      i64.const 7001
      i64.const 0
      call $call-provider
      i64.const 1
      i64.ne
      if
        i32.const -7
        return
      end
      i64.const 0
      call $publish-workspace
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "invoke") (param i64) (param i64) (param i64) (param i64) (result i64)
      i64.const 0)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "call-provider" (func $call-provider-core))
    (export "workspace-len" (func $workspace-len-core))
    (export "workspace-set-len" (func $workspace-set-len-core))
    (export "workspace-write-byte" (func $workspace-write-byte-core))
    (export "publish-workspace" (func $publish-workspace-core)))
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
