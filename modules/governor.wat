(component
  (import "publish-callable" (func $publish-callable (param "slot" u64) (result s32)))
  (core func $publish-callable-core (canon lower (func $publish-callable)))
  (core module $module
    (import "host" "publish-callable" (func $publish-callable (param i64) (result i32)))
    (global $allowed (mut i64) (i64.const 0))
    (func (export "start") (param $config i64) (result i64)
      local.get $config
      global.set $allowed
      i64.const 1)
    (func (export "step") (param i64) (result i32)
      (local $status i32)
      i64.const 2
      call $publish-callable
      local.tee $status
      i32.eqz
      if (result i32)
        i32.const 1
      else
        i32.const 0
        local.get $status
        i32.sub
      end)
    (func (export "invoke") (param i64) (param $operation i64)
      (param $arg0 i64) (param i64) (result i64)
      local.get $operation
      i64.const 1
      i64.ne
      if
        i64.const 0
        return
      end
      local.get $arg0
      global.get $allowed
      i64.eq
      if (result i64)
        i64.const 1
      else
        i64.const 0
      end)
    (func (export "drop") (param i64)))
  (core instance $host
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
