(component
  (import "snapshot-len" (func $snapshot-len (param "index" u64) (result s64)))
  (import "snapshot-byte" (func $snapshot-byte (param "index" u64) (param "offset" u64) (result s32)))
  (import "publish-callable" (func $publish-callable (param "slot" u64) (result s32)))
  (core func $snapshot-len-core (canon lower (func $snapshot-len)))
  (core func $snapshot-byte-core (canon lower (func $snapshot-byte)))
  (core func $publish-callable-core (canon lower (func $publish-callable)))
  (core module $module
    (import "host" "snapshot-len" (func $snapshot-len (param i64) (result i64)))
    (import "host" "snapshot-byte" (func $snapshot-byte (param i64 i64) (result i32)))
    (import "host" "publish-callable" (func $publish-callable (param i64) (result i32)))
    (global $expected (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $expected
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $length i64)
      (local $offset i64)
      (local $hash i64)
      (local $byte i32)
      i64.const 1
      call $snapshot-len
      local.tee $length
      i64.const 0
      i64.lt_s
      if
        local.get $length
        i32.wrap_i64
        return
      end
      i64.const 14695981039346656037
      local.set $hash
      block $done
        loop $scan
          local.get $offset
          local.get $length
          i64.ge_u
          br_if $done
          i64.const 1
          local.get $offset
          call $snapshot-byte
          local.tee $byte
          i32.const 0
          i32.lt_s
          if
            local.get $byte
            return
          end
          local.get $hash
          local.get $byte
          i64.extend_i32_u
          i64.xor
          i64.const 1099511628211
          i64.mul
          local.set $hash
          local.get $offset
          i64.const 1
          i64.add
          local.set $offset
          br $scan
        end
      end
      local.get $hash
      global.get $expected
      i64.ne
      if
        i32.const -4
        return
      end
      i64.const 11
      call $publish-callable
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const -3
      end)
    (func (export "invoke") (param i64) (param $operation i64)
      (param i64) (param i64) (result i64)
      local.get $operation
      i64.const 1
      i64.ne
      if
        i64.const -3
        return
      end
      i64.const 21483426414593)
    (func (export "drop") (param i64)))
  (core instance $host
    (export "snapshot-len" (func $snapshot-len-core))
    (export "snapshot-byte" (func $snapshot-byte-core))
    (export "publish-callable" (func $publish-callable-core)))
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
