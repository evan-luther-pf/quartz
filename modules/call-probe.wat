(component
  (import "resolve" (func $resolve (param "slot" u64) (result s64)))
  (core func $resolve-core (canon lower (func $resolve)))
  (core module $module
    (import "host" "resolve" (func $resolve (param i64) (result i64)))
    (global $count (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $count
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $index i64)
      (local $value i64)
      block $done
        loop $again
          local.get $index
          global.get $count
          i64.ge_u
          br_if $done
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
          local.get $index
          i64.const 1
          i64.add
          local.set $index
          br $again
        end
      end
      i32.const 1)
    (func (export "drop") (param i64)))
  (core instance $host (export "resolve" (func $resolve-core)))
  (core instance $instance (instantiate $module (with "host" (instance $host))))
  (func (export "start") (param "config" u64) (result u64)
    (canon lift (core func $instance "start")))
  (func (export "step") (param "instance" u64) (result s32)
    (canon lift (core func $instance "step")))
  (func (export "drop") (param "instance" u64)
    (canon lift (core func $instance "drop"))))
